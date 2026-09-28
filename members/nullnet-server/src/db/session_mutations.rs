use super::{SessionGeo, models::NewSessionRow, schema::sessions};
use diesel::prelude::*;

pub(super) struct SessionTransaction<'a> {
    pub(super) conn: &'a mut SqliteConnection,
}

impl SessionTransaction<'_> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open(
        &mut self,
        direction: &str,
        stack: &str,
        service: &str,
        net_id: u32,
        peer_ip: &str,
        geo: &SessionGeo,
        blocked: bool,
        detail: &str,
        timestamp: i64,
    ) -> Result<i64, diesel::result::Error> {
        let new_row = NewSessionRow {
            direction: direction.to_owned(),
            stack: stack.to_owned(),
            service: service.to_owned(),
            net_id: net_id as i32,
            peer_ip: peer_ip.to_owned(),
            country_code: geo.country_code.clone(),
            asn: geo.asn.clone(),
            org: geo.org.clone(),
            blocked,
            detail: detail.to_owned(),
            started_at: timestamp,
            last_seen: timestamp,
            ended_at: None,
            history_token: None,
        };
        self.insert(&new_row)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn close_backend(
        &mut self,
        id: i64,
        timestamp: i64,
    ) -> Result<usize, diesel::result::Error> {
        let conn = &mut *self.conn;
        diesel::update(
            sessions::table
                .filter(sessions::id.eq(id))
                .filter(sessions::ended_at.is_null()),
        )
        .set((
            sessions::ended_at.eq(Some(timestamp)),
            sessions::last_seen.eq(timestamp),
        ))
        .execute(&mut *conn)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn touch(
        &mut self,
        direction: &str,
        net_id: u32,
        peer_ip: &str,
        geo: &SessionGeo,
        blocked: bool,
        timestamp: i64,
    ) -> Result<usize, diesel::result::Error> {
        let conn = &mut *self.conn;
        let updated = diesel::update(
            sessions::table
                .filter(sessions::direction.eq(direction.to_owned()))
                .filter(sessions::net_id.eq(net_id as i32))
                .filter(sessions::peer_ip.eq(peer_ip.to_owned()))
                .filter(sessions::ended_at.is_null()),
        )
        .set((
            sessions::last_seen.eq(timestamp),
            sessions::blocked.eq(blocked),
        ))
        .execute(&mut *conn)?;
        if updated > 0 && geo.country_code.is_some() {
            diesel::update(
                sessions::table
                    .filter(sessions::direction.eq(direction.to_owned()))
                    .filter(sessions::net_id.eq(net_id as i32))
                    .filter(sessions::peer_ip.eq(peer_ip.to_owned()))
                    .filter(sessions::ended_at.is_null())
                    .filter(sessions::country_code.is_null()),
            )
            .set((
                sessions::country_code.eq(geo.country_code.clone()),
                sessions::asn.eq(geo.asn.clone()),
                sessions::org.eq(geo.org.clone()),
            ))
            .execute(&mut *conn)?;
        }
        Ok(updated)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_blocked_ingress(
        &mut self,
        stack: &str,
        service: &str,
        peer_ip: &str,
        geo: &SessionGeo,
        detail: &str,
        started_at: i64,
        last_seen: i64,
    ) -> Result<(), diesel::result::Error> {
        let conn = &mut *self.conn;
        let updated = diesel::update(
            sessions::table
                .filter(sessions::direction.eq("ingress"))
                .filter(sessions::blocked.eq(true))
                .filter(sessions::stack.eq(stack.to_owned()))
                .filter(sessions::service.eq(service.to_owned()))
                .filter(sessions::peer_ip.eq(peer_ip.to_owned()))
                .filter(sessions::started_at.eq(started_at)),
        )
        .set((
            sessions::detail.eq(detail.to_owned()),
            sessions::last_seen.eq(last_seen),
            sessions::ended_at.eq(Some(last_seen)),
        ))
        .execute(&mut *conn)?;
        if updated > 0 {
            // Geo resolves asynchronously, so the burst's first write usually
            // predates it. Fill it in, never clear it — same rule as `touch`.
            if geo.country_code.is_some() {
                diesel::update(
                    sessions::table
                        .filter(sessions::direction.eq("ingress"))
                        .filter(sessions::blocked.eq(true))
                        .filter(sessions::stack.eq(stack.to_owned()))
                        .filter(sessions::service.eq(service.to_owned()))
                        .filter(sessions::peer_ip.eq(peer_ip.to_owned()))
                        .filter(sessions::started_at.eq(started_at))
                        .filter(sessions::country_code.is_null()),
                )
                .set((
                    sessions::country_code.eq(geo.country_code.clone()),
                    sessions::asn.eq(geo.asn.clone()),
                    sessions::org.eq(geo.org.clone()),
                ))
                .execute(&mut *conn)?;
            }
            return Ok(());
        }
        let new_row = NewSessionRow {
            direction: "ingress".into(),
            stack: stack.to_owned(),
            service: service.to_owned(),
            net_id: 0,
            peer_ip: peer_ip.to_owned(),
            country_code: geo.country_code.clone(),
            asn: geo.asn.clone(),
            org: geo.org.clone(),
            blocked: true,
            detail: detail.to_owned(),
            started_at,
            last_seen,
            ended_at: Some(last_seen),
            history_token: None,
        };
        diesel::insert_into(sessions::table)
            .values(&new_row)
            .execute(&mut *conn)?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn close_ingress(
        &mut self,
        net_id: u32,
        service: &str,
        peer_ip: &str,
        timestamp: i64,
    ) -> Result<usize, diesel::result::Error> {
        let conn = &mut *self.conn;
        diesel::update(
            sessions::table
                .filter(sessions::direction.eq("ingress"))
                .filter(sessions::net_id.eq(net_id as i32))
                .filter(sessions::service.eq(service.to_owned()))
                .filter(sessions::peer_ip.eq(peer_ip.to_owned()))
                .filter(sessions::ended_at.is_null()),
        )
        .set(sessions::ended_at.eq(Some(timestamp)))
        .execute(&mut *conn)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn extend_ended_egress_dst(
        &mut self,
        net_id: u32,
        peer_ip: &str,
        blocked: bool,
        timestamp: i64,
    ) -> Result<usize, diesel::result::Error> {
        let conn = &mut *self.conn;
        let Some(id) = sessions::table
            .filter(sessions::direction.eq("egress"))
            .filter(sessions::net_id.eq(net_id as i32))
            .filter(sessions::peer_ip.eq(peer_ip.to_owned()))
            .filter(sessions::ended_at.is_not_null())
            .order(sessions::id.desc())
            .select(sessions::id)
            .first::<i64>(&mut *conn)
            .optional()?
        else {
            return Ok(0);
        };
        diesel::update(sessions::table.filter(sessions::id.eq(id)))
            .set((
                sessions::last_seen.eq(timestamp),
                sessions::ended_at.eq(Some(timestamp)),
                sessions::blocked.eq(blocked),
            ))
            .execute(&mut *conn)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn close_egress_dst(
        &mut self,
        net_id: u32,
        peer_ip: &str,
        timestamp: i64,
    ) -> Result<usize, diesel::result::Error> {
        let conn = &mut *self.conn;
        diesel::update(
            sessions::table
                .filter(sessions::direction.eq("egress"))
                .filter(sessions::net_id.eq(net_id as i32))
                .filter(sessions::peer_ip.eq(peer_ip.to_owned()))
                .filter(sessions::ended_at.is_null()),
        )
        .set(sessions::ended_at.eq(Some(timestamp)))
        .execute(&mut *conn)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn close_egress_edge(
        &mut self,
        net_id: u32,
        timestamp: i64,
    ) -> Result<usize, diesel::result::Error> {
        let conn = &mut *self.conn;
        diesel::update(
            sessions::table
                .filter(sessions::direction.eq("egress"))
                .filter(sessions::net_id.eq(net_id as i32))
                .filter(sessions::ended_at.is_null()),
        )
        .set(sessions::ended_at.eq(Some(timestamp)))
        .execute(&mut *conn)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn close_all_open(
        &mut self,
        timestamp: i64,
    ) -> Result<usize, diesel::result::Error> {
        let conn = &mut *self.conn;
        diesel::update(sessions::table.filter(sessions::ended_at.is_null()))
            .set(sessions::ended_at.eq(Some(timestamp)))
            .execute(&mut *conn)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum SessionMutation {
    Open(NewSessionRow),
    Blocked(NewSessionRow),
    Egress {
        row: NewSessionRow,
        active: bool,
        timestamp: i64,
    },
    CloseIngress {
        net_id: u32,
        service: String,
        peer: String,
        timestamp: i64,
    },
    CloseBackend {
        token: uuid::Uuid,
        timestamp: i64,
    },
    CloseEgress {
        net_id: u32,
        timestamp: i64,
    },
    CloseAll(i64),
    Interrupt(i64),
}

impl SessionTransaction<'_> {
    fn insert(&mut self, row: &NewSessionRow) -> QueryResult<i64> {
        diesel::insert_into(sessions::table)
            .values(row)
            .returning(sessions::id)
            .get_result(self.conn)
    }

    pub(super) fn apply(&mut self, mutation: &SessionMutation) -> QueryResult<()> {
        match mutation {
            SessionMutation::Open(row) => {
                self.insert(row)?;
            }
            SessionMutation::Blocked(row) => {
                self.record_blocked_ingress(
                    &row.stack,
                    &row.service,
                    &row.peer_ip,
                    &row.geo(),
                    &row.detail,
                    row.started_at,
                    row.last_seen,
                )?;
            }
            SessionMutation::Egress {
                row,
                active,
                timestamp,
            } => {
                if self.touch(
                    "egress",
                    row.net_id as u32,
                    &row.peer_ip,
                    &row.geo(),
                    row.blocked,
                    row.last_seen,
                )? > 0
                {
                    if !active {
                        self.close_egress_dst(row.net_id as u32, &row.peer_ip, *timestamp)?;
                    }
                } else if *active
                    || self.extend_ended_egress_dst(
                        row.net_id as u32,
                        &row.peer_ip,
                        row.blocked,
                        row.last_seen,
                    )? == 0
                {
                    self.insert(row)?;
                    if !active {
                        self.close_egress_dst(row.net_id as u32, &row.peer_ip, row.last_seen)?;
                    }
                }
            }
            SessionMutation::CloseIngress {
                net_id,
                service,
                peer,
                timestamp,
            } => {
                self.close_ingress(*net_id, service, peer, *timestamp)?;
            }
            SessionMutation::CloseBackend { token, timestamp } => {
                diesel::update(
                    sessions::table
                        .filter(sessions::history_token.eq(token.to_string()))
                        .filter(sessions::ended_at.is_null()),
                )
                .set((
                    sessions::ended_at.eq(Some(*timestamp)),
                    sessions::last_seen.eq(*timestamp),
                ))
                .execute(self.conn)?;
            }
            SessionMutation::CloseEgress { net_id, timestamp } => {
                self.close_egress_edge(*net_id, *timestamp)?;
            }
            SessionMutation::CloseAll(timestamp) => {
                self.close_all_open(*timestamp)?;
            }
            SessionMutation::Interrupt(timestamp) => {
                diesel::update(sessions::table.filter(sessions::ended_at.is_null()))
                    .set((
                        sessions::ended_at.eq(Some(*timestamp)),
                        sessions::detail.eq(diesel::dsl::sql::<diesel::sql_types::Text>(
                            "json_set(detail, '$.recording_interrupted', json('true'))",
                        )),
                    ))
                    .execute(self.conn)?;
            }
        }
        Ok(())
    }
}

impl NewSessionRow {
    fn geo(&self) -> SessionGeo {
        SessionGeo {
            country_code: self.country_code.clone(),
            asn: self.asn.clone(),
            org: self.org.clone(),
        }
    }
}
