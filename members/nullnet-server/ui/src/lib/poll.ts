// The task handles request errors; a slow request must finish before the next poll.
export function poll(task: (signal: AbortSignal) => Promise<void>, interval?: number) {
  let controller = new AbortController();
  let timer: ReturnType<typeof setTimeout> | undefined;
  function stop() {
    controller.abort();
    if (timer !== undefined) clearTimeout(timer);
  }
  async function run() {
    const current = controller;
    await task(current.signal);
    if (interval && !current.signal.aborted) timer = setTimeout(() => void run(), interval);
  }
  void run();
  return {
    stop,
    refresh: () => {
      stop();
      controller = new AbortController();
      return run();
    },
  };
}
