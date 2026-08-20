//! One-time import of legacy `./services/<stack>.toml` files into the
//! normalized `stacks`/`services`/... tables (issue #140 — config moves
//! from files to the DB). Mirrors `auth::bootstrap::ensure_admin_exists`'s
//! idempotent-on-every-startup pattern, but checked per stack rather than
//! with a single "any rows exist" gate: a stack already in the DB (imported
//! on a previous boot, or created directly through the UI/API) is left
//! untouched, so this is safe to run on every restart and never clobbers a
//! DB-side edit with a stale file.

use crate::db::Db;
use crate::events::{Event, EventStore};
use crate::services::input::{route_entries_to_inserts, services_to_inserts, validate_stack_toml};
use nullnet_liberror::{Error, ErrorHandler, Location, location};
use std::path::Path;

/// Where `./services/<stack>.toml` lives in production. Parameterized (not
/// hardcoded into `migrate_legacy_toml`) so tests can point at a scratch
/// directory instead of mutating the process's CWD.
pub(crate) const LEGACY_SERVICES_DIR: &str = "./services";

/// Import every not-yet-migrated `<legacy_dir>/<stack>.toml` into the DB. A
/// missing `legacy_dir` (fresh install) is a no-op, not an error. A file
/// that fails validation is left in place — neither imported nor backed up
/// — and reported via `LegacyConfigImportFailed` so it surfaces on the
/// Events tab rather than silently vanishing from the running config.
/// Successfully-imported files move to `<legacy_dir>/.migrated-toml-backup/`:
/// an operator can still diff/restore from there, but editing a file there
/// (or the original path) again has no effect — the DB is the sole source of
/// truth from this point on.
pub(crate) async fn migrate_legacy_toml(
    db: &Db,
    events: &EventStore,
    legacy_dir: &str,
) -> Result<(), Error> {
    let mut entries = match tokio::fs::read_dir(legacy_dir).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).handle_err(location!()),
    };

    let backup_dir = Path::new(legacy_dir).join(".migrated-toml-backup");
    let mut imported = 0u32;
    let mut skipped = 0u32;
    while let Some(entry) = entries.next_entry().await.handle_err(location!())? {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        let Some(stack) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            continue;
        };

        if db.stacks().exists(&stack).await? {
            continue; // already migrated (or created directly in the DB)
        }

        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "[config migration] failed to read '{}': {e:?}",
                    path.display()
                );
                skipped += 1;
                continue;
            }
        };
        let (_, _, route_entries) = match validate_stack_toml(&content) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "[config migration] '{stack}' failed validation, left on disk for manual fix: {e}"
                );
                events
                    .emit(Event::legacy_config_import_failed(stack, e))
                    .await;
                skipped += 1;
                continue;
            }
        };
        // `validate_stack_toml` already confirmed the TOML parses and is
        // semantically valid; this is a second, independent parse purely to
        // get the exact-as-declared service list (rather than the derived
        // `ServiceInfo` map) to persist — see `stack_services`'s doc comment.
        let services = crate::services::input::stack_services(&content)
            .expect("stack_services can't fail content validate_stack_toml already parsed");

        let service_inserts = services_to_inserts(&services);
        db.stacks().put_services(&stack, &service_inserts).await?;
        let route_inserts = route_entries_to_inserts(&route_entries);
        db.stacks().put_routes(&stack, &route_inserts).await?;

        if let Err(e) = move_to_backup(&path, &stack, &backup_dir).await {
            eprintln!(
                "[config migration] imported '{stack}' but failed to back up its file: {e:?}"
            );
        }
        imported += 1;
    }

    if imported > 0 || skipped > 0 {
        println!(
            "[config migration] imported {imported} stack(s) from {legacy_dir}/*.toml into the \
             database ({skipped} skipped — see warnings above)"
        );
    }
    Ok(())
}

async fn move_to_backup(path: &Path, stack: &str, backup_dir: &Path) -> Result<(), Error> {
    tokio::fs::create_dir_all(backup_dir)
        .await
        .handle_err(location!())?;
    tokio::fs::rename(path, backup_dir.join(format!("{stack}.toml")))
        .await
        .handle_err(location!())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> Db {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "nullnet-server-migrate-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Db::open(dir.join("test.db").to_str().unwrap())
            .await
            .unwrap()
    }

    async fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nullnet-server-migrate-scratch-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        dir
    }

    #[tokio::test]
    async fn missing_services_dir_is_a_noop() {
        let db = test_db().await;
        let events = EventStore::new();
        migrate_legacy_toml(&db, &events, "./no-such-dir-for-this-test")
            .await
            .unwrap();
        assert!(db.stacks().list_stacks().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn imports_valid_files_and_backs_them_up() {
        let dir = scratch_dir("imports").await;
        tokio::fs::write(
            dir.join("alpha.toml"),
            "[[services]]\nname = \"a\"\ntimeout = 0\n",
        )
        .await
        .unwrap();

        let db = test_db().await;
        let events = EventStore::new();
        migrate_legacy_toml(&db, &events, dir.to_str().unwrap())
            .await
            .unwrap();

        let services = db.stacks().services_for("alpha").await.unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "a");
        assert!(!dir.join("alpha.toml").exists());
        assert!(dir.join(".migrated-toml-backup/alpha.toml").exists());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn invalid_file_is_left_in_place_and_not_imported() {
        let dir = scratch_dir("invalid").await;
        tokio::fs::write(dir.join("broken.toml"), "not valid toml [[[")
            .await
            .unwrap();

        let db = test_db().await;
        let events = EventStore::new();
        migrate_legacy_toml(&db, &events, dir.to_str().unwrap())
            .await
            .unwrap();

        assert!(!db.stacks().exists("broken").await.unwrap());
        assert!(dir.join("broken.toml").exists());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn does_not_overwrite_a_stack_already_in_the_db() {
        let dir = scratch_dir("no-overwrite").await;
        tokio::fs::write(
            dir.join("alpha.toml"),
            "[[services]]\nname = \"from-file\"\ntimeout = 0\n",
        )
        .await
        .unwrap();

        let db = test_db().await;
        let existing = crate::db::ServiceInsert {
            name: "from-db",
            docker_container: None,
            process_path: None,
            port: None,
            timeout: Some(0),
            max_networks: None,
            protocol: None,
            listen_port: None,
            egress_blocked_countries: None,
            egress_allowed_countries: None,
            ingress_blocked_countries: None,
            ingress_allowed_countries: None,
            triggers: Vec::new(),
            dependencies: Vec::new(),
        };
        db.stacks()
            .put_services("alpha", &[existing])
            .await
            .unwrap();

        let events = EventStore::new();
        migrate_legacy_toml(&db, &events, dir.to_str().unwrap())
            .await
            .unwrap();

        let services = db.stacks().services_for("alpha").await.unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "from-db");
        // left on disk untouched — it was never a candidate for import
        assert!(dir.join("alpha.toml").exists());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
