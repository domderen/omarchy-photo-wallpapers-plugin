//! Apple SRP-6a: mathematical helpers adapted from MIT-licensed rhoopr/kei,
//! revision 3b8443b493845d408e36ac267af8360f2665d5e0. See third_party/kei-MIT.txt.
//! The synthetic interoperability fixture is computed by an independent server.
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use num_bigint::BigUint;
use rand::RngExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

const PRIME: &str = concat!(
    "AC6BDB41324A9A9BF166DE5E1389582FAF72B6651987EE07FC319294",
    "3DB56050A37329CBB4A099ED8193E0757767A13DD52312AB4B03310D",
    "CD7F48A9DA04FD50E8083969EDB767B0CF6095179A163AB3661A05FB",
    "D5FAAAE82918A9962F0B93B855F97993EC975EEAA80D740ADBF4FF74",
    "7359D041D5C33EA71D281E446B14773BCA97B43A23FB801676BD207A",
    "436C6481F1D2B9078717461A5B9D32E688F87748544523B524B0D57D",
    "5EA77A2775D2ECFA032CFBDBF52FB3786160279004E57AE6AF874E73",
    "03CE53299CCC041C7BC308D82A5698F3A8D0C38271AE35F8E9DBFBB6",
    "94B5C803D89F7AE435DE236D525F54759B65E372FCD68EF20FA7111F",
    "9E4AFF73"
);

#[derive(Deserialize)]
pub struct Challenge {
    pub salt: String,
    pub b: String,
    pub iteration: u32,
    pub protocol: String,
    pub c: String,
}
pub struct Proof {
    pub m1: String,
    pub m2: String,
    pub key: String,
}
pub struct SrpClient {
    seed: Zeroizing<[u8; 32]>,
    n: BigUint,
    a: BigUint,
}
fn hash(parts: &[&[u8]]) -> Vec<u8> {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().to_vec()
}
fn pad(n: &BigUint) -> Vec<u8> {
    let b = n.to_bytes_be();
    let mut p = vec![0; 256usize.saturating_sub(b.len())];
    p.extend(b);
    p
}

impl SrpClient {
    pub fn random() -> Result<Self> {
        let mut seed = [0; 32];
        rand::rng().fill(&mut seed);
        Self::from_private(seed)
    }
    pub fn from_private(seed: [u8; 32]) -> Result<Self> {
        ensure!(seed.iter().any(|b| *b != 0), "invalid_srp_private_key");
        let n = BigUint::parse_bytes(PRIME.as_bytes(), 16)
            .ok_or_else(|| anyhow::anyhow!("invalid_srp_group"))?;
        let a = BigUint::from(2u32).modpow(&BigUint::from_bytes_be(&seed), &n);
        Ok(Self {
            seed: Zeroizing::new(seed),
            n,
            a,
        })
    }
    pub fn public_a(&self) -> String {
        B64.encode(self.a.to_bytes_be())
    }
    pub fn prove(&self, username: &str, password: &str, c: &Challenge) -> Result<Proof> {
        ensure!(
            (1..=1_000_000).contains(&c.iteration),
            "invalid_srp_iterations"
        );
        ensure!(
            c.protocol == "s2k" || c.protocol == "s2k_fo",
            "unsupported_srp_protocol"
        );
        ensure!(
            c.salt.len() <= 2048 && c.b.len() <= 512 && c.c.len() <= 16384,
            "invalid_srp_challenge"
        );
        let salt = B64
            .decode(&c.salt)
            .map_err(|_| anyhow::anyhow!("invalid_srp_salt"))?;
        let b = BigUint::from_bytes_be(
            &B64.decode(&c.b)
                .map_err(|_| anyhow::anyhow!("invalid_srp_public_key"))?,
        );
        ensure!(&b % &self.n != BigUint::ZERO, "invalid_srp_public_key");
        let password_hash = Zeroizing::new(hash(&[password.as_bytes()]));
        let input = Zeroizing::new(if c.protocol == "s2k_fo" {
            hex::encode(&*password_hash).into_bytes()
        } else {
            password_hash.to_vec()
        });
        let mut key = Zeroizing::new([0; 32]);
        pbkdf2::pbkdf2_hmac::<Sha256>(&input, &salt, c.iteration, &mut *key);
        let x = BigUint::from_bytes_be(&hash(&[&salt, &hash(&[b":", &*key])]));
        let g = BigUint::from(2u32);
        let k = BigUint::from_bytes_be(&hash(&[&self.n.to_bytes_be(), &pad(&g)]));
        let u = BigUint::from_bytes_be(&hash(&[&pad(&self.a), &pad(&b)]));
        ensure!(u != BigUint::ZERO, "invalid_srp_scrambling");
        let v = g.modpow(&x, &self.n);
        let kv = (k * v) % &self.n;
        let base = (&b + &self.n - kv) % &self.n;
        let secret = base.modpow(&(BigUint::from_bytes_be(&*self.seed) + u * x), &self.n);
        let session_key = Zeroizing::new(hash(&[&secret.to_bytes_be()]));
        let xor: Vec<u8> = hash(&[&self.n.to_bytes_be()])
            .iter()
            .zip(hash(&[&pad(&g)]))
            .map(|(a, b)| a ^ b)
            .collect();
        let m1 = hash(&[
            &xor,
            &hash(&[username.as_bytes()]),
            &salt,
            &self.a.to_bytes_be(),
            &b.to_bytes_be(),
            &session_key,
        ]);
        let m2 = hash(&[&self.a.to_bytes_be(), &m1, &session_key]);
        Ok(Proof {
            m1: B64.encode(m1),
            m2: B64.encode(m2),
            key: B64.encode(&*session_key),
        })
    }
}
