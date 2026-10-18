use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod, Runtime};
use rustls::pki_types::{CertificateDer, pem::PemObject};
use std::{path::Path, sync::Arc, time::Duration};
use tokio_postgres_rustls::MakeRustlsConnect;

pub(crate) const RDS_CA_FILE: &str = "/etc/qed/rds-global-bundle.pem";
pub(crate) fn unavailable<T>(_: T) -> String {
    "database unavailable or incompatible".to_owned()
}

pub(crate) fn root_store(extra: Option<&Path>) -> Result<rustls::RootCertStore, String> {
    let mut roots = rustls::RootCertStore::empty();
    for path in ["/etc/ssl/certs/ca-certificates.crt", "/etc/pki/tls/certs/ca-bundle.crt"] {
        if let Ok(certificates) = CertificateDer::pem_file_iter(path) {
            for cert in certificates.flatten() {
                let _ = roots.add(cert);
            }
        }
    }
    if let Some(path) = extra {
        let certificates = CertificateDer::pem_file_iter(path)
            .map_err(|_| "cannot read QED_DATABASE_CA_FILE".to_owned())?;
        let mut count = 0;
        for certificate in certificates {
            roots
                .add(certificate.map_err(|_| "invalid database CA PEM".to_owned())?)
                .map_err(|_| "invalid database CA certificate".to_owned())?;
            count += 1;
        }
        if count == 0 {
            return Err("database CA file contains no certificates".into());
        }
    }
    Ok(roots)
}
pub(crate) fn tls_verifier() -> Result<Arc<rustls::client::WebPkiServerVerifier>, String> {
    let configured = std::env::var_os("QED_DATABASE_CA_FILE");
    let extra = configured
        .as_deref()
        .map(Path::new)
        .or_else(|| Path::new(RDS_CA_FILE).exists().then(|| Path::new(RDS_CA_FILE)));
    rustls::client::WebPkiServerVerifier::builder_with_provider(
        Arc::new(root_store(extra)?),
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .build()
    .map_err(unavailable)
}
pub(crate) fn pool(url: &str) -> Result<Pool, String> {
    // tokio-postgres calls this mode `require`; rustls always verifies both chain and hostname.
    let mut parsed = reqwest::Url::parse(url).map_err(unavailable)?;
    let query: Vec<_> = parsed
        .query_pairs()
        .map(|(key, value)| {
            let value =
                if key == "sslmode" && value == "verify-full" { "require".into() } else { value };
            (key.into_owned(), value.into_owned())
        })
        .collect();
    if !query.is_empty() {
        parsed.query_pairs_mut().clear().extend_pairs(query);
    }
    let mut config: tokio_postgres::Config = parsed.as_str().parse().map_err(unavailable)?;
    config.connect_timeout(Duration::from_secs(2));
    let options = format!("{} -cstatement_timeout=5000", config.get_options().unwrap_or_default());
    config.options(options);
    if config.get_hosts().iter().any(|host| match host {
        tokio_postgres::config::Host::Tcp(host) => {
            !host.eq_ignore_ascii_case("localhost")
                && !host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
        }
        _ => false,
    }) {
        config.ssl_mode(tokio_postgres::config::SslMode::Require);
    }
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(unavailable)?
    .with_webpki_verifier(tls_verifier()?)
    .with_no_client_auth();
    let manager = Manager::from_config(
        config,
        MakeRustlsConnect::new(tls),
        ManagerConfig { recycling_method: RecyclingMethod::Fast },
    );
    Pool::builder(manager)
        .max_size(4)
        .runtime(Runtime::Tokio1)
        .wait_timeout(Some(Duration::from_secs(2)))
        .create_timeout(Some(Duration::from_secs(2)))
        .build()
        .map_err(unavailable)
}

/// Both stores share one ordered migration boundary, including storage-owned indexes.
pub(crate) async fn migrate(client: &mut deadpool_postgres::Client) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    const MIGRATIONS: &[(i32, &str)] = &[
        (1, include_str!("../../migrations/0001_asset_history.sql")),
        (2, include_str!("../../migrations/0002_durable_storage.sql")),
        (3, include_str!("../../migrations/0003_bounded_history.sql")),
        (4, include_str!("../../migrations/0004_storage_recent_index.sql")),
        (5, include_str!("../../migrations/0005_scheduled_projection_policy.sql")),
        (6, include_str!("../../migrations/0006_guard_unknown_tracking.sql")),
    ];
    let tx = client.transaction().await.map_err(unavailable)?;
    tx.batch_execute("SET LOCAL statement_timeout='5s'; SELECT pg_advisory_xact_lock(71511001); CREATE TABLE IF NOT EXISTS schema_migrations(version integer PRIMARY KEY,checksum text NOT NULL,applied_at timestamptz NOT NULL DEFAULT now())").await.map_err(unavailable)?;
    let newest = MIGRATIONS.last().expect("embedded migrations").0;
    if tx
        .query_opt("SELECT version FROM schema_migrations WHERE version>$1 LIMIT 1", &[&newest])
        .await
        .map_err(unavailable)?
        .is_some()
    {
        return Err("database schema is newer than this binary".into());
    }
    for (version, sql) in MIGRATIONS {
        let checksum = crate::domain::attestation::hex_lower(&Sha256::digest(sql.as_bytes()));
        if let Some(row) = tx
            .query_opt("SELECT checksum FROM schema_migrations WHERE version=$1", &[version])
            .await
            .map_err(unavailable)?
        {
            if row.get::<_, String>(0) != checksum {
                return Err("database migration checksum mismatch".into());
            }
        } else {
            tx.batch_execute(sql).await.map_err(unavailable)?;
            tx.execute(
                "INSERT INTO schema_migrations(version,checksum) VALUES($1,$2)",
                &[version, &checksum],
            )
            .await
            .map_err(unavailable)?;
        }
    }
    tx.commit().await.map_err(unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_rds_roots_load_and_invalid_ca_is_rejected() {
        let normal = root_store(None).unwrap().len();
        let roots = root_store(Some(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/rds-global-bundle.pem"
        ))))
        .unwrap();
        assert!(roots.len() > normal);
        let invalid = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            invalid.path(),
            "-----BEGIN CERTIFICATE-----\nYWJj\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        assert!(root_store(Some(invalid.path())).is_err());
        std::fs::write(invalid.path(), "not a certificate").unwrap();
        assert!(root_store(Some(invalid.path())).is_err());
    }
}
