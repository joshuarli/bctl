use anyhow::{Context, Result};
use graviola::signing::ecdsa::{P256, SigningKey};
use rcgen::{KeyPair, RemoteKeyPair};

struct GraviolaP256 {
    key: SigningKey<P256>,
    public: Vec<u8>,
}

impl GraviolaP256 {
    fn new(key: SigningKey<P256>) -> Result<Self> {
        let mut spki = [0u8; 128];
        let spki = key.to_spki_der(&mut spki).context("encode P-256 public key")?;
        let public = spki[spki.len() - 65..].to_vec();
        anyhow::ensure!(
            public.first() == Some(&0x04),
            "P-256 public key is not an uncompressed point"
        );
        Ok(Self { key, public })
    }
}

impl RemoteKeyPair for GraviolaP256 {
    fn public_key(&self) -> &[u8] {
        &self.public
    }

    fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, rcgen::Error> {
        let mut signature = [0u8; 128];
        self.key
            .sign_asn1::<graviola::hashing::Sha256>(&[msg], &mut signature)
            .map(|signature| signature.to_vec())
            .map_err(|_| rcgen::Error::RemoteKeyError)
    }

    fn algorithm(&self) -> &'static rcgen::SignatureAlgorithm {
        &rcgen::PKCS_ECDSA_P256_SHA256
    }
}

pub(crate) fn serial_number() -> rcgen::SerialNumber {
    let mut bytes = [0u8; 20];
    graviola::random::fill(&mut bytes).expect("certificate serial randomness");
    bytes[0] &= 0x7f;
    rcgen::SerialNumber::from_slice(&bytes)
}

pub(crate) struct P256Key {
    pair: KeyPair,
    pkcs8: Vec<u8>,
}

impl P256Key {
    pub(crate) fn generate() -> Result<Self> {
        loop {
            let mut scalar = [0u8; 32];
            graviola::random::fill(&mut scalar).context("generate P-256 scalar")?;
            let mut sec1 = vec![0x30, 0x25, 0x02, 0x01, 0x01, 0x04, 0x20];
            sec1.extend_from_slice(&scalar);
            let Ok(signing) = SigningKey::<P256>::from_sec1_der(&sec1) else {
                continue;
            };
            let mut buffer = [0u8; 256];
            let pkcs8 = signing
                .to_pkcs8_der(&mut buffer)
                .context("encode P-256 key")?
                .to_vec();
            let pair = KeyPair::from_remote(Box::new(GraviolaP256::new(signing)?))
                .context("build remote P-256 key")?;
            return Ok(Self { pair, pkcs8 });
        }
    }

    pub(crate) fn from_pkcs8_der(bytes: &[u8]) -> Result<Self> {
        let signing = SigningKey::<P256>::from_pkcs8_der(bytes)
            .context("parse P-256 PKCS#8 key")?;
        let pair = KeyPair::from_remote(Box::new(GraviolaP256::new(signing)?))
            .context("build remote P-256 key")?;
        Ok(Self {
            pair,
            pkcs8: bytes.to_vec(),
        })
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.starts_with(b"-----BEGIN") {
            let key = rustls_pemfile::private_key(&mut &bytes[..])
                .context("parse CA key PEM")?
                .context("CA key PEM contains no private key")?;
            match key {
                rustls_pki_types::PrivateKeyDer::Pkcs8(key) => {
                    Self::from_pkcs8_der(key.secret_pkcs8_der())
                }
                rustls_pki_types::PrivateKeyDer::Sec1(key) => {
                    let signing = SigningKey::<P256>::from_sec1_der(key.secret_sec1_der())
                        .context("parse P-256 SEC1 key")?;
                    let mut buffer = [0u8; 256];
                    let pkcs8 = signing
                        .to_pkcs8_der(&mut buffer)
                        .context("encode P-256 key")?
                        .to_vec();
                    let pair = KeyPair::from_remote(Box::new(GraviolaP256::new(signing)?))
                        .context("build remote P-256 key")?;
                    Ok(Self { pair, pkcs8 })
                }
                _ => anyhow::bail!("CA key is not a P-256 private key"),
            }
        } else {
            Self::from_pkcs8_der(bytes)
        }
    }

    pub(crate) fn key_pair(&self) -> &KeyPair {
        &self.pair
    }

    pub(crate) fn key_identifier_method(&self) -> rcgen::KeyIdMethod {
        let spki = self.pair.public_key_der();
        let digest = <graviola::hashing::Sha256 as graviola::hashing::Hash>::hash(&spki);
        let digest: &[u8] = digest.as_ref();
        rcgen::KeyIdMethod::PreSpecified(digest[..20].to_vec())
    }

    pub(crate) fn serialize_der(&self) -> Vec<u8> {
        self.pkcs8.clone()
    }
}
