//! Renders a real `odoo.conf` file. Kept separate from `odoo.rs` on purpose
//! — that module supervises an arbitrary process and deliberately knows
//! nothing Odoo-specific (see its own module doc comment); this is the one
//! place that owns actual Odoo config-file knowledge: `addons_path` is a
//! comma-joined list in load order, and a local Postgres connection is
//! addressed via a Unix-socket directory as `db_host` — exactly how
//! `pg_admin::PgConnInfo` already talks to the same cluster (see its doc
//! comment), so Odoo and this project's own tooling agree on how to reach
//! Postgres.
//!
//! One thing deliberately **not** here: `dev_mode`. Odoo lists it in
//! `blacklist_for_save` ("not exposed in the configuration file") and
//! `_parse_config` recomputes `options['dev_mode']` from the command-line
//! `--dev` *after* reading the file, so a `dev_mode = reload` line in
//! `odoo.conf` is read and then thrown away — verified against a real
//! Odoo 17: with the key in the file and no watcher module installed, Odoo
//! logged no warning at all, because the reload branch was never reached.
//! Auto-reload therefore goes on the command line, in `start_server`.

use std::path::Path;

pub struct OdooConfParams<'a> {
    /// Addons directories in load order — rank 0 first, matching
    /// `AddonsSource::rank` and the shadowing rule `modules.rs` implements.
    pub addons_path: Vec<String>,
    /// The Postgres cluster's Unix-socket directory (its data dir).
    pub db_host: &'a Path,
    pub db_port: u16,
    pub db_user: &'a str,
    pub http_port: u16,
    /// Odoo's own `data_dir` (filestore, sessions) — separate from the
    /// Postgres data dir above.
    pub data_dir: &'a Path,
}

pub fn render(params: &OdooConfParams) -> String {
    format!(
        "[options]\n\
         addons_path = {}\n\
         data_dir = {}\n\
         db_host = {}\n\
         db_port = {}\n\
         db_user = {}\n\
         db_password = False\n\
         http_port = {}\n\
         dbfilter = ^%d$\n\
         admin_passwd = admin\n\
         without_demo = True\n\
         list_db = True\n",
        params.addons_path.join(","),
        params.data_dir.display(),
        params.db_host.display(),
        params.db_port,
        params.db_user,
        params.http_port,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn renders_addons_path_in_given_order_comma_joined() {
        let data_dir = PathBuf::from("/app/servers/1/filestore");
        let db_host = PathBuf::from("/app/pg/data");
        let out = render(&OdooConfParams {
            addons_path: vec!["/checkout/addons".to_string(), "/oca/repo".to_string()],
            db_host: &db_host,
            db_port: 55432,
            db_user: "postgres",
            http_port: 8069,
            data_dir: &data_dir,
        });
        assert!(out.contains("addons_path = /checkout/addons,/oca/repo"));
        assert!(out.contains("db_host = /app/pg/data"));
        assert!(out.contains("db_port = 55432"));
        assert!(out.contains("db_user = postgres"));
        assert!(out.contains("http_port = 8069"));
        assert!(out.contains("data_dir = /app/servers/1/filestore"));
        assert!(out.starts_with("[options]\n"));
    }

    #[test]
    fn renders_a_single_addons_path_entry_without_a_trailing_comma() {
        let data_dir = PathBuf::from("/data");
        let db_host = PathBuf::from("/pg");
        let out = render(&OdooConfParams {
            addons_path: vec!["/only/one".to_string()],
            db_host: &db_host,
            db_port: 5432,
            db_user: "postgres",
            http_port: 8069,
            data_dir: &data_dir,
        });
        assert!(out.contains("addons_path = /only/one\n"));
    }

    /// The whole address story of this product — `acme.localhost:8069` opens
    /// the `acme` database, with no database picker in the way — is one line
    /// of config. Odoo's default `dbfilter` is the empty string, which
    /// filters nothing (see `db_filter` in Odoo's own `http.py`), so leaving
    /// it out means every address on the port shows the selector instead.
    /// This assertion exists because that was exactly the bug: the proxy,
    /// the model doc comments and the UI all described `^%d$` routing while
    /// the generated file never asked for it.
    #[test]
    fn always_asks_odoo_to_route_by_the_first_label_of_the_host() {
        let data_dir = PathBuf::from("/data");
        let db_host = PathBuf::from("/pg");
        let out = render(&OdooConfParams {
            addons_path: vec!["/only/one".to_string()],
            db_host: &db_host,
            db_port: 5432,
            db_user: "odoo",
            http_port: 8069,
            data_dir: &data_dir,
        });
        assert!(out.contains("\ndbfilter = ^%d$\n"), "generated conf was:\n{out}");
    }

}
