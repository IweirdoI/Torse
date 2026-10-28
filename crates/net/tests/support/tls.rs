//! Disposable local certificate material, generated rather than embedded in source.
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub struct Fixture {
    pub root: PathBuf,
    temporary: bool,
}
impl Fixture {
    pub fn temporary() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "torse-tls-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut fixture = Self::create(&root);
        fixture.temporary = true;
        fixture
    }
    pub fn create(root: &Path) -> Self {
        std::fs::create_dir(root).unwrap();
        let ca_key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca = params.self_signed(&ca_key).unwrap();
        std::fs::write(root.join("ca.pem"), ca.pem()).unwrap();
        let issuer = Issuer::from_params(&params, &ca_key);
        for (name, usage, expired) in [
            ("server", ExtendedKeyUsagePurpose::ServerAuth, false),
            ("alice", ExtendedKeyUsagePurpose::ClientAuth, false),
            ("bob", ExtendedKeyUsagePurpose::ClientAuth, false),
            ("expired", ExtendedKeyUsagePurpose::ClientAuth, true),
            ("expired-server", ExtendedKeyUsagePurpose::ServerAuth, true),
        ] {
            let mut leaf = CertificateParams::new(vec!["localhost".into()]).unwrap();
            leaf.extended_key_usages = vec![usage];
            leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            if expired {
                leaf.not_before = rcgen::date_time_ymd(1990, 1, 1);
                leaf.not_after = rcgen::date_time_ymd(2000, 1, 1);
            }
            let key = KeyPair::generate().unwrap();
            let certificate = leaf.signed_by(&key, &issuer).unwrap();
            std::fs::write(root.join(format!("{name}.pem")), certificate.pem()).unwrap();
            std::fs::write(root.join(format!("{name}.key")), key.serialize_pem()).unwrap();
        }
        Self {
            root: root.to_owned(),
            temporary: false,
        }
    }
    pub fn path(&self, file: &str) -> PathBuf {
        self.root.join(file)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if self.temporary {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}
