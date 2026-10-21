use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const PRESSURE_WINDOW: Duration = Duration::from_secs(1);
const COMPLETION_HISTORY: Duration = Duration::from_secs(10);
const RECOVERY_QUIET: Duration = Duration::from_secs(5);
const CONGESTED_WINDOWS: u8 = 2;
const RECOVERY_WINDOWS: u8 = 2;
const BUCKET_BURST: f64 = 32.0;
const PRESSURE_HISTORY: usize = 60;
const TRANSITION_HISTORY: usize = 20;

#[derive(Clone, Copy, Debug)]
pub(crate) struct ProxyLoad {
    pub(crate) epoch: u64,
    pub(crate) arrivals: u64,
    pub(crate) completions: u64,
    pub(crate) waiters: u32,
    pub(crate) wait_us: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trend {
    Congested,
    Healthy,
    Neutral,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PressureTrend {
    Growing,
    Healthy,
    Neutral,
}

#[derive(Clone, Debug)]
pub(crate) struct PressureWindowSnapshot {
    pub(crate) age: Duration,
    pub(crate) duration: Duration,
    pub(crate) arrivals: u64,
    pub(crate) completions: u64,
    pub(crate) pending: u32,
    pub(crate) mean_latency_us: u64,
    pub(crate) trend: PressureTrend,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ControlMode {
    Normal,
    Multiplexing,
    Recovery,
}

#[derive(Clone, Debug)]
pub(crate) struct ControlTransitionSnapshot {
    pub(crate) mode: ControlMode,
    pub(crate) age: Duration,
    pub(crate) source_proxy: IpAddr,
}

#[derive(Clone, Debug)]
pub(crate) struct HostStatusSnapshot {
    pub(crate) mode: ControlMode,
    pub(crate) state_age: Option<Duration>,
    pub(crate) rate_per_sec: Option<f64>,
    pub(crate) tokens: Option<f64>,
    pub(crate) last_token_shortage_age: Option<Duration>,
    pub(crate) source_proxy: Option<IpAddr>,
    pub(crate) transitions: Vec<ControlTransitionSnapshot>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct NodeCongestionSnapshot {
    pub(crate) proxy_pressure_age: Option<Duration>,
    pub(crate) proxy_windows: Vec<PressureWindowSnapshot>,
    pub(crate) setup_pressure_age: Option<Duration>,
    pub(crate) setup_windows: Vec<PressureWindowSnapshot>,
    pub(crate) control: Option<HostStatusSnapshot>,
}

#[derive(Clone, Copy)]
struct PressureWindow {
    ended_at: Instant,
    duration: Duration,
    arrivals: u64,
    completions: u64,
    pending: u32,
    mean_latency_us: u64,
    trend: PressureTrend,
}

#[derive(Clone, Copy)]
struct PressureView {
    sequence: u64,
    trend: Trend,
}

#[derive(Clone, Copy)]
struct LoadSample {
    epoch: u64,
    arrivals: u64,
    completions: u64,
    pending: u32,
}

struct Pressure {
    window_started: Instant,
    start: LoadSample,
    last: LoadSample,
    latency_sum: u128,
    latency_samples: u64,
    previous_latency_mean: Option<u64>,
    stalled_is_growing: bool,
    carry_boundary_latency: bool,
    congested_windows: u8,
    sequence: u64,
    trend: Trend,
    last_observed: Instant,
    history: VecDeque<PressureWindow>,
}

impl Pressure {
    fn new(
        now: Instant,
        sample: LoadSample,
        latency_us: Option<u64>,
        stalled_is_growing: bool,
        carry_boundary_latency: bool,
    ) -> Self {
        Self {
            window_started: now,
            start: sample,
            last: sample,
            latency_sum: latency_us.map_or(0, u128::from),
            latency_samples: u64::from(latency_us.is_some()),
            previous_latency_mean: None,
            stalled_is_growing,
            carry_boundary_latency,
            congested_windows: 0,
            sequence: 0,
            trend: Trend::Neutral,
            last_observed: now,
            history: VecDeque::with_capacity(PRESSURE_HISTORY),
        }
    }

    fn reset(&mut self, now: Instant, sample: LoadSample, latency_us: Option<u64>) {
        let sequence = self.sequence.saturating_add(1);
        let stalled_is_growing = self.stalled_is_growing;
        let carry_boundary_latency = self.carry_boundary_latency;
        let history =
            std::mem::replace(&mut self.history, VecDeque::with_capacity(PRESSURE_HISTORY));
        *self = Self::new(
            now,
            sample,
            latency_us,
            stalled_is_growing,
            carry_boundary_latency,
        );
        self.sequence = sequence;
        self.history = history;
    }

    fn observe(&mut self, now: Instant, sample: LoadSample, latency_us: Option<u64>) {
        if sample.epoch != self.last.epoch {
            self.reset(now, sample, latency_us);
            return;
        }
        if sample.arrivals < self.last.arrivals || sample.completions < self.last.completions {
            return;
        }
        self.last = sample;
        self.last_observed = now;
        if let Some(latency_us) = latency_us {
            self.latency_sum += u128::from(latency_us);
            self.latency_samples += 1;
        }
        if now.duration_since(self.window_started) < PRESSURE_WINDOW {
            return;
        }

        let arrivals = self.last.arrivals - self.start.arrivals;
        let completions = self.last.completions - self.start.completions;
        let latency_mean = if self.latency_samples == 0 {
            self.previous_latency_mean.unwrap_or_default()
        } else {
            u64::try_from(self.latency_sum / u128::from(self.latency_samples)).unwrap_or(u64::MAX)
        };
        let latency_rising = self
            .previous_latency_mean
            .is_none_or(|previous| latency_mean > previous);
        let latency_not_rising = self.previous_latency_mean.is_none_or(|previous| {
            latency_mean <= previous.saturating_add(previous / 10).saturating_add(100)
        });
        let stalled = self.stalled_is_growing && arrivals > completions && completions == 0;
        let growing = arrivals > completions
            && self.last.pending > self.start.pending
            && (latency_rising || stalled);
        let healthy = arrivals <= completions
            && self.last.pending <= self.start.pending
            && latency_not_rising;

        let display_trend = if growing {
            PressureTrend::Growing
        } else if healthy {
            PressureTrend::Healthy
        } else {
            PressureTrend::Neutral
        };

        self.trend = if growing {
            self.congested_windows = self.congested_windows.saturating_add(1);
            if self.congested_windows >= CONGESTED_WINDOWS {
                self.congested_windows = 0;
                Trend::Congested
            } else {
                Trend::Neutral
            }
        } else if healthy {
            self.congested_windows = 0;
            Trend::Healthy
        } else {
            self.congested_windows = 0;
            Trend::Neutral
        };
        self.sequence += 1;
        if self.history.len() == PRESSURE_HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(PressureWindow {
            ended_at: now,
            duration: now.duration_since(self.window_started),
            arrivals,
            completions,
            pending: self.last.pending,
            mean_latency_us: latency_mean,
            trend: display_trend,
        });
        self.previous_latency_mean = Some(latency_mean);
        self.window_started = now;
        self.start = sample;
        self.last = sample;
        self.latency_sum = if self.carry_boundary_latency {
            latency_us.map_or(0, u128::from)
        } else {
            0
        };
        self.latency_samples = u64::from(self.carry_boundary_latency && latency_us.is_some());
    }

    fn view(&self) -> PressureView {
        PressureView {
            sequence: self.sequence,
            trend: self.trend,
        }
    }

    fn current_window(&self, now: Instant) -> Option<PressureWindow> {
        let arrivals = self.last.arrivals - self.start.arrivals;
        let completions = self.last.completions - self.start.completions;
        if arrivals == 0 && completions == 0 && self.last.pending == self.start.pending {
            return None;
        }
        let elapsed = now.duration_since(self.window_started);
        let duration = elapsed.min(PRESSURE_WINDOW);
        if duration.is_zero() {
            return None;
        }
        let mean_latency_us = if self.latency_samples == 0 {
            self.previous_latency_mean.unwrap_or_default()
        } else {
            u64::try_from(self.latency_sum / u128::from(self.latency_samples)).unwrap_or(u64::MAX)
        };
        Some(PressureWindow {
            ended_at: self.window_started + duration,
            duration,
            arrivals,
            completions,
            pending: self.last.pending,
            mean_latency_us,
            trend: PressureTrend::Neutral,
        })
    }
}

struct SetupPressure {
    pressure: Pressure,
    arrivals: u64,
    completions: u64,
    pending: u32,
}

impl SetupPressure {
    fn new(now: Instant) -> Self {
        Self {
            pressure: Pressure::new(
                now,
                LoadSample {
                    epoch: 0,
                    arrivals: 0,
                    completions: 0,
                    pending: 0,
                },
                None,
                true,
                false,
            ),
            arrivals: 0,
            completions: 0,
            pending: 0,
        }
    }

    fn sample(&self) -> LoadSample {
        LoadSample {
            epoch: 0,
            arrivals: self.arrivals,
            completions: self.completions,
            pending: self.pending,
        }
    }

    fn started(&mut self, now: Instant) {
        self.arrivals = self.arrivals.saturating_add(1);
        self.pending = self.pending.saturating_add(1);
        self.pressure.observe(now, self.sample(), None);
    }

    fn completed(&mut self, now: Instant, latency: Duration) {
        self.completions = self.completions.saturating_add(1);
        self.pending = self.pending.saturating_sub(1);
        let latency_us = u64::try_from(latency.as_micros()).unwrap_or(u64::MAX);
        self.pressure.observe(now, self.sample(), Some(latency_us));
    }
}

#[derive(Clone, Copy)]
struct ControlTransition {
    at: Instant,
    mode: ControlMode,
    source_proxy: IpAddr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PressureSource {
    Proxy(IpAddr),
    Setup(IpAddr),
}

struct HostRate {
    rate: Option<f64>,
    tokens: f64,
    last_refill: Instant,
    last_denied: Option<Instant>,
    completions: VecDeque<Instant>,
    pressure_sequences: HashMap<PressureSource, u64>,
    healthy_windows: u8,
    state_since: Option<Instant>,
    source_proxy: Option<IpAddr>,
    transitions: VecDeque<ControlTransition>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum RateTransition {
    Activated { host: IpAddr },
    RecoveryStarted { host: IpAddr },
    Recovered { host: IpAddr },
}

pub(crate) struct AdmissionDecision {
    pub(crate) dedicated: bool,
    pub(crate) transitions: Vec<RateTransition>,
}

impl HostRate {
    fn new(now: Instant) -> Self {
        Self {
            rate: None,
            tokens: 0.0,
            last_refill: now,
            last_denied: None,
            completions: VecDeque::new(),
            pressure_sequences: HashMap::new(),
            healthy_windows: 0,
            state_since: None,
            source_proxy: None,
            transitions: VecDeque::with_capacity(TRANSITION_HISTORY),
        }
    }

    fn mode(&self) -> ControlMode {
        match self.rate {
            None => ControlMode::Normal,
            Some(0.0) => ControlMode::Multiplexing,
            Some(_) => ControlMode::Recovery,
        }
    }

    fn transition(&mut self, mode: ControlMode, proxy: IpAddr, now: Instant) {
        self.state_since = Some(now);
        self.source_proxy = (mode != ControlMode::Normal).then_some(proxy);
        if self.transitions.len() == TRANSITION_HISTORY {
            self.transitions.pop_front();
        }
        self.transitions.push_back(ControlTransition {
            at: now,
            mode,
            source_proxy: proxy,
        });
    }

    fn trim_completions(&mut self, now: Instant) {
        while self
            .completions
            .front()
            .is_some_and(|at| now.duration_since(*at) > COMPLETION_HISTORY)
        {
            self.completions.pop_front();
        }
    }

    fn record_completion(&mut self, now: Instant) {
        self.trim_completions(now);
        if self
            .completions
            .back()
            .is_some_and(|last| now.duration_since(*last) > RECOVERY_QUIET)
        {
            self.completions.clear();
        }
        self.completions.push_back(now);
    }

    fn sustained_completions(&mut self, now: Instant) -> bool {
        self.trim_completions(now);
        self.completions.front().is_some_and(|first| {
            now.duration_since(*first) >= RECOVERY_QUIET
                && self
                    .completions
                    .back()
                    .is_some_and(|last| now.duration_since(*last) <= RECOVERY_QUIET)
        })
    }

    fn denied(&mut self, now: Instant) {
        self.last_denied = Some(now);
        self.completions.clear();
    }

    fn apply_all(
        &mut self,
        host: IpAddr,
        proxy: IpAddr,
        pressures: &[(PressureSource, PressureView)],
        now: Instant,
    ) -> Option<RateTransition> {
        let changed = pressures.iter().any(|(source, pressure)| {
            pressure.sequence
                > self
                    .pressure_sequences
                    .get(source)
                    .copied()
                    .unwrap_or_default()
        });
        if !changed {
            return None;
        }
        for (source, pressure) in pressures {
            self.pressure_sequences.insert(*source, pressure.sequence);
        }
        let trend = if pressures
            .iter()
            .any(|(_, pressure)| pressure.trend == Trend::Congested)
        {
            Trend::Congested
        } else if pressures
            .iter()
            .all(|(_, pressure)| pressure.trend == Trend::Healthy)
        {
            Trend::Healthy
        } else {
            Trend::Neutral
        };
        match trend {
            Trend::Congested => {
                let activating = self.rate != Some(0.0);
                self.rate = Some(0.0);
                self.tokens = 0.0;
                self.last_refill = now;
                self.denied(now);
                self.healthy_windows = 0;
                if activating {
                    self.transition(ControlMode::Multiplexing, proxy, now);
                    return Some(RateTransition::Activated { host });
                }
            }
            Trend::Healthy => {
                if self.rate.is_some() {
                    self.healthy_windows = self.healthy_windows.saturating_add(1);
                    if self.healthy_windows < RECOVERY_WINDOWS {
                        return None;
                    }
                    let recovery_starting = self.rate == Some(0.0);
                    *self.rate.as_mut().expect("rate is active") += 1.0;
                    let recovered = self
                        .last_denied
                        .is_some_and(|last| now.duration_since(last) >= RECOVERY_QUIET)
                        && self.tokens >= BUCKET_BURST
                        && self.sustained_completions(now);
                    if recovered {
                        self.rate = None;
                        self.tokens = 0.0;
                        self.transition(ControlMode::Normal, proxy, now);
                        return Some(RateTransition::Recovered { host });
                    }
                    if recovery_starting {
                        self.transition(ControlMode::Recovery, proxy, now);
                        return Some(RateTransition::RecoveryStarted { host });
                    }
                }
            }
            Trend::Neutral => {
                if self.rate == Some(0.0) {
                    self.healthy_windows = 0;
                }
            }
        }
        None
    }

    #[cfg(test)]
    fn apply(
        &mut self,
        host: IpAddr,
        proxy: IpAddr,
        pressure: PressureView,
        now: Instant,
    ) -> Option<RateTransition> {
        self.apply_all(
            host,
            proxy,
            &[(PressureSource::Proxy(proxy), pressure)],
            now,
        )
    }

    fn refill(&mut self, now: Instant) {
        let Some(rate) = self.rate else {
            self.last_refill = now;
            return;
        };
        self.tokens = (self.tokens + now.duration_since(self.last_refill).as_secs_f64() * rate)
            .min(BUCKET_BURST);
        self.last_refill = now;
    }
}

#[derive(Default)]
struct State {
    proxies: HashMap<IpAddr, Pressure>,
    setups: HashMap<IpAddr, SetupPressure>,
    hosts: HashMap<IpAddr, HostRate>,
}

#[derive(Default)]
pub(crate) struct CongestionController {
    state: Mutex<State>,
}

pub(crate) struct SetupAttempt {
    controller: std::sync::Arc<CongestionController>,
    host: IpAddr,
    started: Instant,
}

impl Drop for SetupAttempt {
    fn drop(&mut self) {
        let now = Instant::now();
        let mut state = self
            .controller
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .setups
            .get_mut(&self.host)
            .expect("setup attempt is registered")
            .completed(now, now.duration_since(self.started));
    }
}

impl CongestionController {
    pub(crate) fn observe_proxy(&self, proxy: IpAddr, sample: ProxyLoad) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::observe_proxy_locked(&mut state, proxy, sample, Instant::now());
    }

    fn observe_proxy_locked(state: &mut State, proxy: IpAddr, sample: ProxyLoad, now: Instant) {
        let load = LoadSample {
            epoch: sample.epoch,
            arrivals: sample.arrivals,
            completions: sample.completions,
            pending: sample.waiters,
        };
        state
            .proxies
            .entry(proxy)
            .and_modify(|pressure| pressure.observe(now, load, Some(sample.wait_us)))
            .or_insert_with(|| Pressure::new(now, load, Some(sample.wait_us), false, true));
    }

    pub(crate) fn setup_attempt(self: &std::sync::Arc<Self>, host: IpAddr) -> SetupAttempt {
        let now = Instant::now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .setups
            .entry(host)
            .or_insert_with(|| SetupPressure::new(now))
            .started(now);
        drop(state);
        SetupAttempt {
            controller: self.clone(),
            host,
            started: now,
        }
    }

    pub(crate) fn allow_dedicated(&self, proxy: IpAddr, destination: IpAddr) -> AdmissionDecision {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::allow_dedicated_locked(&mut state, proxy, destination, Instant::now())
    }

    fn allow_dedicated_locked(
        state: &mut State,
        proxy: IpAddr,
        destination: IpAddr,
        now: Instant,
    ) -> AdmissionDecision {
        let proxy_pressure = state.proxies.get(&proxy).map(Pressure::view);
        let setup_pressure = state
            .setups
            .get(&destination)
            .map(|setup| setup.pressure.view());
        let hosts = [proxy, destination];
        let host_count = if proxy == destination { 1 } else { 2 };
        let mut transitions = Vec::new();
        for host in &hosts[..host_count] {
            let rate = state
                .hosts
                .entry(*host)
                .or_insert_with(|| HostRate::new(now));
            rate.refill(now);
            let mut pressures = [(
                PressureSource::Proxy(proxy),
                PressureView {
                    sequence: 0,
                    trend: Trend::Neutral,
                },
            ); 2];
            let mut pressure_count = 0;
            if *host == proxy
                && let Some(pressure) = proxy_pressure
            {
                pressures[pressure_count] = (PressureSource::Proxy(proxy), pressure);
                pressure_count += 1;
            }
            if *host == destination
                && let Some(pressure) = setup_pressure
            {
                pressures[pressure_count] = (PressureSource::Setup(destination), pressure);
                pressure_count += 1;
            }
            if let Some(transition) =
                rate.apply_all(*host, proxy, &pressures[..pressure_count], now)
            {
                transitions.push(transition);
            }
        }

        let allowed = hosts[..host_count].iter().all(|host| {
            state.hosts[host]
                .rate
                .is_none_or(|_| state.hosts[host].tokens >= 1.0)
        });
        if allowed {
            for host in &hosts[..host_count] {
                let rate = state.hosts.get_mut(host).expect("host rate exists");
                if rate.rate.is_some() {
                    rate.tokens -= 1.0;
                }
            }
        } else {
            for host in &hosts[..host_count] {
                let rate = state.hosts.get_mut(host).expect("host rate exists");
                if rate.rate.is_some() && rate.tokens < 1.0 {
                    rate.denied(now);
                }
            }
        }
        AdmissionDecision {
            dedicated: allowed,
            transitions,
        }
    }

    pub(crate) fn record_dedicated_completion(&self, proxy: IpAddr, destination: IpAddr) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::record_dedicated_completion_locked(&mut state, proxy, destination, Instant::now());
    }

    fn record_dedicated_completion_locked(
        state: &mut State,
        proxy: IpAddr,
        destination: IpAddr,
        now: Instant,
    ) {
        for host in [proxy, destination] {
            let rate = state
                .hosts
                .entry(host)
                .or_insert_with(|| HostRate::new(now));
            rate.record_completion(now);
            if proxy == destination {
                break;
            }
        }
    }

    pub(crate) fn snapshots(
        &self,
        nodes: impl IntoIterator<Item = IpAddr>,
    ) -> HashMap<IpAddr, NodeCongestionSnapshot> {
        let now = Instant::now();
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        nodes
            .into_iter()
            .map(|node| {
                let proxy_pressure = state.proxies.get(&node);
                let proxy_windows = proxy_pressure
                    .into_iter()
                    .flat_map(|pressure| pressure.history.iter())
                    .copied()
                    .chain(proxy_pressure.and_then(|pressure| pressure.current_window(now)))
                    .map(|window| PressureWindowSnapshot {
                        age: now.duration_since(window.ended_at),
                        duration: window.duration,
                        arrivals: window.arrivals,
                        completions: window.completions,
                        pending: window.pending,
                        mean_latency_us: window.mean_latency_us,
                        trend: window.trend,
                    })
                    .collect();
                let setup_pressure = state.setups.get(&node);
                let setup_windows = setup_pressure
                    .into_iter()
                    .flat_map(|setup| setup.pressure.history.iter())
                    .copied()
                    .chain(setup_pressure.and_then(|setup| setup.pressure.current_window(now)))
                    .map(|window| PressureWindowSnapshot {
                        age: now.duration_since(window.ended_at),
                        duration: window.duration,
                        arrivals: window.arrivals,
                        completions: window.completions,
                        pending: window.pending,
                        mean_latency_us: window.mean_latency_us,
                        trend: window.trend,
                    })
                    .collect();
                let control = state.hosts.get(&node).map(|host| {
                    let rate = host.rate;
                    let tokens = rate.map(|rate| {
                        (host.tokens + now.duration_since(host.last_refill).as_secs_f64() * rate)
                            .min(BUCKET_BURST)
                    });
                    HostStatusSnapshot {
                        mode: host.mode(),
                        state_age: host.state_since.map(|at| now.duration_since(at)),
                        rate_per_sec: rate,
                        tokens,
                        last_token_shortage_age: host.last_denied.map(|at| now.duration_since(at)),
                        source_proxy: host.source_proxy,
                        transitions: host
                            .transitions
                            .iter()
                            .rev()
                            .map(|transition| ControlTransitionSnapshot {
                                mode: transition.mode,
                                age: now.duration_since(transition.at),
                                source_proxy: transition.source_proxy,
                            })
                            .collect(),
                    }
                });
                (
                    node,
                    NodeCongestionSnapshot {
                        proxy_pressure_age: proxy_pressure
                            .map(|pressure| now.duration_since(pressure.last_observed)),
                        proxy_windows,
                        setup_pressure_age: setup_pressure
                            .map(|setup| now.duration_since(setup.pressure.last_observed)),
                        setup_windows,
                        control,
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([192, 0, 2, last])
    }

    fn sample(arrivals: u64, completions: u64, waiters: u32, wait_us: u64) -> ProxyLoad {
        ProxyLoad {
            epoch: 1,
            arrivals,
            completions,
            waiters,
            wait_us,
        }
    }

    fn congest_setup(state: &mut State, host: IpAddr, start: Instant) {
        let setup = state
            .setups
            .entry(host)
            .or_insert_with(|| SetupPressure::new(start));
        setup.started(start);
        setup.started(start + Duration::from_secs(1));
        setup.started(start + Duration::from_secs(2));
    }

    #[test]
    fn stable_waiters_do_not_activate_control() {
        let controller = CongestionController::default();
        let start = Instant::now();
        let mut state = controller.state.lock().unwrap();
        CongestionController::observe_proxy_locked(&mut state, ip(1), sample(0, 0, 4, 500), start);
        CongestionController::observe_proxy_locked(
            &mut state,
            ip(1),
            sample(100, 100, 4, 500),
            start + Duration::from_secs(1),
        );
        CongestionController::observe_proxy_locked(
            &mut state,
            ip(1),
            sample(200, 200, 4, 500),
            start + Duration::from_secs(2),
        );
        assert!(
            CongestionController::allow_dedicated_locked(
                &mut state,
                ip(1),
                ip(2),
                start + Duration::from_secs(2)
            )
            .dedicated
        );
    }

    #[test]
    fn pressure_history_is_bounded_and_survives_proxy_epoch_changes() {
        let start = Instant::now();
        let first = sample(0, 0, 0, 0);
        let mut pressure = Pressure::new(
            start,
            LoadSample {
                epoch: first.epoch,
                arrivals: first.arrivals,
                completions: first.completions,
                pending: first.waiters,
            },
            Some(first.wait_us),
            false,
            true,
        );
        for second in 1..=65 {
            let next = sample(second * 10, second * 10, 0, 100);
            pressure.observe(
                start + Duration::from_secs(second),
                LoadSample {
                    epoch: next.epoch,
                    arrivals: next.arrivals,
                    completions: next.completions,
                    pending: next.waiters,
                },
                Some(next.wait_us),
            );
        }
        assert_eq!(pressure.history.len(), PRESSURE_HISTORY);
        assert_eq!(pressure.history.front().unwrap().arrivals, 10);

        let mut reset = sample(1, 1, 0, 100);
        reset.epoch = 2;
        pressure.observe(
            start + Duration::from_secs(66),
            LoadSample {
                epoch: reset.epoch,
                arrivals: reset.arrivals,
                completions: reset.completions,
                pending: reset.waiters,
            },
            Some(reset.wait_us),
        );
        assert_eq!(pressure.history.len(), PRESSURE_HISTORY);
    }

    #[test]
    fn growing_proxy_backlog_activates_only_proxy_control() {
        let controller = CongestionController::default();
        let start = Instant::now();
        let mut state = controller.state.lock().unwrap();
        for i in 0..100 {
            CongestionController::record_dedicated_completion_locked(
                &mut state,
                ip(1),
                ip(2),
                start + Duration::from_millis(i * 10),
            );
        }
        CongestionController::observe_proxy_locked(&mut state, ip(1), sample(0, 0, 0, 0), start);
        CongestionController::observe_proxy_locked(
            &mut state,
            ip(1),
            sample(100, 70, 30, 1_000),
            start + Duration::from_secs(1),
        );
        CongestionController::observe_proxy_locked(
            &mut state,
            ip(1),
            sample(200, 140, 60, 2_000),
            start + Duration::from_secs(2),
        );
        let first = CongestionController::allow_dedicated_locked(
            &mut state,
            ip(1),
            ip(2),
            start + Duration::from_secs(2),
        );
        assert!(!first.dedicated);
        assert_eq!(
            first.transitions,
            vec![RateTransition::Activated { host: ip(1) }]
        );
        assert!(
            !CongestionController::allow_dedicated_locked(
                &mut state,
                ip(1),
                ip(2),
                start + Duration::from_secs(2)
            )
            .dedicated
        );
    }

    #[test]
    fn proxy_backlog_does_not_activate_an_unmeasured_destination() {
        let controller = CongestionController::default();
        let start = Instant::now();
        let mut state = controller.state.lock().unwrap();
        CongestionController::observe_proxy_locked(&mut state, ip(1), sample(0, 0, 0, 0), start);
        CongestionController::observe_proxy_locked(
            &mut state,
            ip(1),
            sample(100, 70, 30, 1_000),
            start + Duration::from_secs(1),
        );
        CongestionController::observe_proxy_locked(
            &mut state,
            ip(1),
            sample(200, 140, 60, 2_000),
            start + Duration::from_secs(2),
        );

        let decision = CongestionController::allow_dedicated_locked(
            &mut state,
            ip(1),
            ip(2),
            start + Duration::from_secs(2),
        );

        assert_eq!(
            decision.transitions,
            vec![RateTransition::Activated { host: ip(1) }]
        );
        assert_eq!(state.hosts[&ip(2)].mode(), ControlMode::Normal);
    }

    #[test]
    fn destination_setup_backlog_activates_only_its_node() {
        let controller = CongestionController::default();
        let start = Instant::now();
        let mut state = controller.state.lock().unwrap();
        congest_setup(&mut state, ip(2), start);

        let decision = CongestionController::allow_dedicated_locked(
            &mut state,
            ip(1),
            ip(2),
            start + Duration::from_secs(2),
        );

        assert!(!decision.dedicated);
        assert_eq!(
            decision.transitions,
            vec![RateTransition::Activated { host: ip(2) }]
        );
        assert_eq!(state.hosts[&ip(1)].mode(), ControlMode::Normal);
    }

    #[test]
    fn destination_pressure_never_spills_into_another_destination() {
        let controller = CongestionController::default();
        let start = Instant::now();
        let mut state = controller.state.lock().unwrap();
        congest_setup(&mut state, ip(2), start);

        let decision = CongestionController::allow_dedicated_locked(
            &mut state,
            ip(1),
            ip(3),
            start + Duration::from_secs(2),
        );

        assert!(decision.dedicated);
        assert!(decision.transitions.is_empty());
        assert_eq!(state.hosts[&ip(3)].mode(), ControlMode::Normal);
    }

    #[test]
    fn cross_host_setup_requires_both_independent_controllers() {
        let controller = CongestionController::default();
        let start = Instant::now();
        let mut state = controller.state.lock().unwrap();
        CongestionController::observe_proxy_locked(&mut state, ip(1), sample(0, 0, 0, 0), start);
        CongestionController::observe_proxy_locked(
            &mut state,
            ip(1),
            sample(100, 70, 30, 1_000),
            start + Duration::from_secs(1),
        );
        CongestionController::observe_proxy_locked(
            &mut state,
            ip(1),
            sample(200, 140, 60, 2_000),
            start + Duration::from_secs(2),
        );
        congest_setup(&mut state, ip(2), start);

        let decision = CongestionController::allow_dedicated_locked(
            &mut state,
            ip(1),
            ip(2),
            start + Duration::from_secs(2),
        );

        assert!(!decision.dedicated);
        assert_eq!(
            decision.transitions,
            vec![
                RateTransition::Activated { host: ip(1) },
                RateTransition::Activated { host: ip(2) },
            ]
        );
    }

    #[test]
    fn setup_latency_must_worsen_while_completions_continue() {
        let start = Instant::now();
        let mut pressure = Pressure::new(
            start,
            LoadSample {
                epoch: 0,
                arrivals: 0,
                completions: 0,
                pending: 0,
            },
            None,
            true,
            false,
        );
        let load = |arrivals, completions, pending| LoadSample {
            epoch: 0,
            arrivals,
            completions,
            pending,
        };
        pressure.observe(start + Duration::from_secs(1), load(10, 5, 5), Some(1_000));
        pressure.observe(start + Duration::from_secs(2), load(20, 10, 10), Some(500));
        assert_eq!(pressure.view().trend, Trend::Neutral);
        pressure.observe(
            start + Duration::from_secs(3),
            load(30, 15, 15),
            Some(1_000),
        );
        pressure.observe(
            start + Duration::from_secs(4),
            load(40, 20, 20),
            Some(2_000),
        );
        assert_eq!(pressure.view().trend, Trend::Congested);
    }

    #[test]
    fn setup_attempts_complete_independently_per_host() {
        let controller = std::sync::Arc::new(CongestionController::default());
        let proxy_attempt = controller.setup_attempt(ip(1));
        let destination_attempt = controller.setup_attempt(ip(2));
        {
            let state = controller.state.lock().unwrap();
            assert_eq!(state.setups[&ip(1)].arrivals, 1);
            assert_eq!(state.setups[&ip(2)].arrivals, 1);
            assert_eq!(state.setups[&ip(1)].pending, 1);
            assert_eq!(state.setups[&ip(2)].pending, 1);
        }
        drop(destination_attempt);
        {
            let state = controller.state.lock().unwrap();
            assert_eq!(state.setups[&ip(1)].completions, 0);
            assert_eq!(state.setups[&ip(2)].completions, 1);
            assert_eq!(state.setups[&ip(1)].pending, 1);
            assert_eq!(state.setups[&ip(2)].pending, 0);
        }
        drop(proxy_attempt);
        assert_eq!(controller.state.lock().unwrap().setups[&ip(1)].pending, 0);
    }

    #[test]
    fn separate_same_host_setup_operations_are_counted() {
        let controller = std::sync::Arc::new(CongestionController::default());
        let first = controller.setup_attempt(ip(1));
        let second = controller.setup_attempt(ip(1));
        assert_eq!(controller.state.lock().unwrap().setups[&ip(1)].arrivals, 2);
        drop((first, second));
        assert_eq!(
            controller.state.lock().unwrap().setups[&ip(1)].completions,
            2
        );
    }

    #[test]
    fn new_proxy_epoch_discards_old_pressure_and_can_reactivate() {
        let controller = CongestionController::default();
        let start = Instant::now();
        let mut state = controller.state.lock().unwrap();
        CongestionController::observe_proxy_locked(
            &mut state,
            ip(1),
            sample(100, 50, 50, 1_000),
            start,
        );
        CongestionController::observe_proxy_locked(
            &mut state,
            ip(1),
            sample(200, 100, 100, 2_000),
            start + Duration::from_secs(1),
        );
        assert!(
            CongestionController::allow_dedicated_locked(
                &mut state,
                ip(1),
                ip(2),
                start + Duration::from_secs(1)
            )
            .dedicated
        );
        let mut reset = sample(1, 0, 0, 0);
        reset.epoch = 2;
        CongestionController::observe_proxy_locked(
            &mut state,
            ip(1),
            reset,
            start + Duration::from_secs(2),
        );
        let mut growing = sample(101, 70, 30, 1_000);
        growing.epoch = 2;
        CongestionController::observe_proxy_locked(
            &mut state,
            ip(1),
            growing,
            start + Duration::from_secs(3),
        );
        growing = sample(201, 140, 60, 2_000);
        growing.epoch = 2;
        CongestionController::observe_proxy_locked(
            &mut state,
            ip(1),
            growing,
            start + Duration::from_secs(4),
        );
        let decision = CongestionController::allow_dedicated_locked(
            &mut state,
            ip(1),
            ip(2),
            start + Duration::from_secs(4),
        );
        assert_eq!(decision.transitions.len(), 1);
    }

    #[test]
    fn out_of_order_sample_does_not_reset_pressure() {
        let controller = CongestionController::default();
        let start = Instant::now();
        let mut state = controller.state.lock().unwrap();
        CongestionController::observe_proxy_locked(&mut state, ip(1), sample(0, 0, 0, 0), start);
        CongestionController::observe_proxy_locked(
            &mut state,
            ip(1),
            sample(100, 70, 30, 1_000),
            start + Duration::from_secs(1),
        );
        CongestionController::observe_proxy_locked(
            &mut state,
            ip(1),
            sample(50, 40, 10, 500),
            start + Duration::from_millis(1_500),
        );
        CongestionController::observe_proxy_locked(
            &mut state,
            ip(1),
            sample(200, 140, 60, 2_000),
            start + Duration::from_secs(2),
        );
        let decision = CongestionController::allow_dedicated_locked(
            &mut state,
            ip(1),
            ip(2),
            start + Duration::from_secs(2),
        );
        assert_eq!(decision.transitions.len(), 1);
    }

    #[test]
    fn congestion_pauses_dedicated_setups_then_additively_probes() {
        let start = Instant::now();
        let mut rate = HostRate::new(start);
        let proxy = ip(1);
        assert_eq!(
            rate.apply(
                ip(2),
                proxy,
                PressureView {
                    sequence: 1,
                    trend: Trend::Congested,
                },
                start + Duration::from_secs(1),
            ),
            Some(RateTransition::Activated { host: ip(2) })
        );
        rate.apply(
            ip(2),
            proxy,
            PressureView {
                sequence: 2,
                trend: Trend::Congested,
            },
            start + Duration::from_secs(2),
        );
        assert_eq!(rate.rate, Some(0.0));
        rate.apply(
            ip(2),
            proxy,
            PressureView {
                sequence: 3,
                trend: Trend::Healthy,
            },
            start + Duration::from_secs(3),
        );
        assert_eq!(rate.rate, Some(0.0));
        rate.apply(
            ip(2),
            proxy,
            PressureView {
                sequence: 4,
                trend: Trend::Neutral,
            },
            start + Duration::from_secs(4),
        );
        rate.apply(
            ip(2),
            proxy,
            PressureView {
                sequence: 5,
                trend: Trend::Healthy,
            },
            start + Duration::from_secs(5),
        );
        assert_eq!(rate.rate, Some(0.0));
        assert_eq!(
            rate.apply(
                ip(2),
                proxy,
                PressureView {
                    sequence: 6,
                    trend: Trend::Healthy,
                },
                start + Duration::from_secs(6),
            ),
            Some(RateTransition::RecoveryStarted { host: ip(2) })
        );
        assert_eq!(rate.rate, Some(1.0));
        assert_eq!(
            rate.apply(
                ip(2),
                proxy,
                PressureView {
                    sequence: 7,
                    trend: Trend::Congested,
                },
                start + Duration::from_secs(7),
            ),
            Some(RateTransition::Activated { host: ip(2) })
        );
        assert_eq!(rate.rate, Some(0.0));
        rate.apply(
            ip(2),
            proxy,
            PressureView {
                sequence: 8,
                trend: Trend::Healthy,
            },
            start + Duration::from_secs(8),
        );
        rate.apply(
            ip(2),
            proxy,
            PressureView {
                sequence: 9,
                trend: Trend::Healthy,
            },
            start + Duration::from_secs(9),
        );
        assert_eq!(rate.rate, Some(1.0));
        rate.tokens = 1.0;
        rate.last_refill = start + Duration::from_secs(12);
        assert_eq!(
            rate.apply(
                ip(2),
                proxy,
                PressureView {
                    sequence: 10,
                    trend: Trend::Healthy,
                },
                start + Duration::from_secs(12),
            ),
            None
        );
        assert_eq!(rate.rate, Some(2.0));
        rate.tokens = BUCKET_BURST;
        rate.record_completion(start + Duration::from_secs(8));
        rate.record_completion(start + Duration::from_secs(13));
        assert_eq!(
            rate.apply(
                ip(2),
                proxy,
                PressureView {
                    sequence: 11,
                    trend: Trend::Healthy,
                },
                start + Duration::from_secs(13),
            ),
            Some(RateTransition::Recovered { host: ip(2) })
        );
        assert_eq!(rate.rate, None);
    }

    #[test]
    fn one_off_setup_is_visible_without_waiting_for_another_setup() {
        let start = Instant::now();
        let mut setup = SetupPressure::new(start);
        setup.started(start);
        setup.completed(start + Duration::from_millis(20), Duration::from_millis(20));

        let window = setup
            .pressure
            .current_window(start + Duration::from_secs(5))
            .expect("one-off setup has a display window");
        assert_eq!(window.duration, PRESSURE_WINDOW);
        assert_eq!(window.ended_at, start + PRESSURE_WINDOW);
        assert_eq!(window.arrivals, 1);
        assert_eq!(window.completions, 1);
        assert_eq!(window.pending, 0);
        assert_eq!(window.mean_latency_us, 20_000);
    }

    #[test]
    fn idle_time_does_not_remove_the_probe_limit() {
        let start = Instant::now();
        let mut rate = HostRate::new(start);
        rate.rate = Some(1.0);
        rate.tokens = BUCKET_BURST;
        rate.last_denied = Some(start);
        rate.healthy_windows = RECOVERY_WINDOWS - 1;
        rate.record_completion(start + Duration::from_secs(1));

        assert_eq!(
            rate.apply(
                ip(2),
                ip(1),
                PressureView {
                    sequence: 1,
                    trend: Trend::Healthy,
                },
                start + Duration::from_secs(15),
            ),
            None
        );
        assert_eq!(rate.rate, Some(2.0));
    }
}
