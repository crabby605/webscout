//! Crypto primitives behind the prelude's `crypto.subtle`. Bytes cross as Uint8Array in and
//! ArrayBuffer out; RSA/EC keys cross as JWK JSON (the prelude's canonical key form).
//! Every function returns null on bad input, which the prelude turns into the DOMException Chrome throws.

use aes_gcm::aead::{Aead, AeadCore, KeyInit, Payload, consts::U12, generic_array::GenericArray};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use hmac::{Mac, SimpleHmac, digest::{Digest, DynDigest, core_api::BlockSizeUser}};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey, signature::hazmat::{PrehashSigner, PrehashVerifier}};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use rand::thread_rng;
use rquickjs::{ArrayBuffer, Ctx, Function, Object, TypedArray};
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey};
use rsa::traits::{PrivateKeyParts, PublicKeyParts};
use rsa::{BigUint, Oaep, RsaPrivateKey, RsaPublicKey};
use serde::Deserialize;
use serde_json::json;

trait Hash: Digest + DynDigest + BlockSizeUser + Clone + Send + Sync + 'static {}
impl<T: Digest + DynDigest + BlockSizeUser + Clone + Send + Sync + 'static> Hash for T {}

/// Calls `f::<Hash>(args)` for a WebCrypto hash name ("SHA-256"); None for any other name.
macro_rules! with_hash {
    ($name:expr, $f:ident($($arg:expr),*)) => {
        match $name.as_str() {
            "SHA-1" => $f::<sha1::Sha1>($($arg),*),
            "SHA-256" => $f::<sha2::Sha256>($($arg),*),
            "SHA-384" => $f::<sha2::Sha384>($($arg),*),
            "SHA-512" => $f::<sha2::Sha512>($($arg),*),
            _ => None,
        }
    };
}

type Bytes<'js> = TypedArray<'js, u8>;

fn bytes(a: &Bytes) -> Vec<u8> {
    // SAFETY: copied out before any JS runs, so the engine can't detach or resize the buffer under us.
    a.as_raw().map(|p| unsafe { p.as_ref() }.to_vec()).unwrap_or_default()
}

fn buf<'js>(ctx: &Ctx<'js>, v: Option<Vec<u8>>) -> rquickjs::Result<Option<ArrayBuffer<'js>>> {
    v.map(|v| ArrayBuffer::new(ctx.clone(), v)).transpose()
}

fn digest<D: Hash>(data: &[u8]) -> Option<Vec<u8>> {
    Some(<D as Digest>::digest(data).to_vec())
}

fn hmac<D: Hash>(key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    let mut m = <SimpleHmac<D> as KeyInit>::new_from_slice(key).ok()?;
    Mac::update(&mut m, data);
    Some(m.finalize().into_bytes().to_vec())
}

fn pbkdf2<D: Hash>(password: &[u8], salt: &[u8], rounds: u32, len: usize) -> Option<Vec<u8>> {
    let mut out = vec![0; len];
    pbkdf2::pbkdf2::<SimpleHmac<D>>(password, salt, rounds, &mut out).ok()?;
    Some(out)
}

// ponytail: 96-bit IVs and 128-bit tags only (what nearly every site uses); add AesGcm<_, U16> etc. if one doesn't.
fn aes_gcm(encrypt: bool, key: &[u8], iv: &[u8], aad: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    fn run<C: KeyInit + Aead + AeadCore<NonceSize = U12>>(encrypt: bool, key: &[u8], iv: &[u8], payload: Payload) -> Option<Vec<u8>> {
        let c = C::new_from_slice(key).ok()?;
        let iv = GenericArray::from_slice(iv);
        if encrypt { c.encrypt(iv, payload) } else { c.decrypt(iv, payload) }.ok()
    }
    if iv.len() != 12 { return None; }
    let payload = Payload { msg: data, aad };
    match key.len() {
        16 => run::<aes_gcm::Aes128Gcm>(encrypt, key, iv, payload),
        32 => run::<aes_gcm::Aes256Gcm>(encrypt, key, iv, payload),
        _ => None,
    }
}

#[derive(Deserialize)]
struct RsaJwk { n: String, e: String, d: Option<String>, p: Option<String>, q: Option<String> }

fn big(s: &str) -> Option<BigUint> {
    Some(BigUint::from_bytes_be(&B64.decode(s).ok()?))
}

fn b64(n: &BigUint) -> String {
    B64.encode(n.to_bytes_be())
}

fn rsa_public(jwk: &str) -> Option<RsaPublicKey> {
    let j: RsaJwk = serde_json::from_str(jwk).ok()?;
    RsaPublicKey::new(big(&j.n)?, big(&j.e)?).ok()
}

fn rsa_private(jwk: &str) -> Option<RsaPrivateKey> {
    let j: RsaJwk = serde_json::from_str(jwk).ok()?;
    let primes = match (j.p, j.q) {
        (Some(p), Some(q)) => vec![big(&p)?, big(&q)?],
        _ => vec![],
    };
    RsaPrivateKey::from_components(big(&j.n)?, big(&j.e)?, big(&j.d?)?, primes).ok()
}

fn rsa_public_jwk(k: &RsaPublicKey) -> String {
    json!({"kty": "RSA", "n": b64(k.n()), "e": b64(k.e())}).to_string()
}

fn rsa_private_jwk(k: &RsaPrivateKey) -> Option<String> {
    let [p, q] = k.primes() else { return None };
    Some(json!({
        "kty": "RSA", "n": b64(k.n()), "e": b64(k.e()), "d": b64(k.d()), "p": b64(p), "q": b64(q),
        "dp": b64(k.dp()?), "dq": b64(k.dq()?), "qi": b64(&k.crt_coefficient()?),
    }).to_string())
}

fn is_private(jwk: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(jwk).is_ok_and(|v| v.get("d").is_some())
}

/// spki / pkcs8 / raw (EC public point) / jwk bytes → canonical JWK JSON (private if the input was).
fn jwk_from(kty: &str, format: &str, data: &[u8]) -> Option<String> {
    match (kty, format) {
        ("RSA", "spki") => Some(rsa_public_jwk(&RsaPublicKey::from_public_key_der(data).ok()?)),
        ("RSA", "pkcs8") => rsa_private_jwk(&RsaPrivateKey::from_pkcs8_der(data).ok()?),
        ("RSA", "jwk") => {
            let s = std::str::from_utf8(data).ok()?;
            if is_private(s) { rsa_private_jwk(&rsa_private(s)?) } else { Some(rsa_public_jwk(&rsa_public(s)?)) }
        }
        ("EC", "spki") => Some(p256::PublicKey::from_public_key_der(data).ok()?.to_jwk_string()),
        ("EC", "raw") => Some(p256::PublicKey::from_sec1_bytes(data).ok()?.to_jwk_string()),
        ("EC", "pkcs8") => Some(p256::SecretKey::from_pkcs8_der(data).ok()?.to_jwk_string().to_string()),
        ("EC", "jwk") => {
            // elliptic-curve rejects unknown members, but browser JWKs carry ext/key_ops/alg.
            let mut v: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(data).ok()?;
            v.retain(|k, _| matches!(k.as_str(), "kty" | "crv" | "x" | "y" | "d"));
            let s = &serde_json::Value::Object(v).to_string();
            if is_private(s) { Some(p256::SecretKey::from_jwk_str(s).ok()?.to_jwk_string().to_string()) }
            else { Some(p256::PublicKey::from_jwk_str(s).ok()?.to_jwk_string()) }
        }
        _ => None,
    }
}

/// Canonical JWK JSON → spki / pkcs8 / raw (EC public point) bytes.
fn jwk_to(format: &str, jwk: &str) -> Option<Vec<u8>> {
    let rsa = serde_json::from_str::<serde_json::Value>(jwk).ok()?["kty"] == "RSA";
    match (rsa, format) {
        (true, "spki") => Some(rsa_public(jwk)?.to_public_key_der().ok()?.into_vec()),
        (true, "pkcs8") => Some(rsa_private(jwk)?.to_pkcs8_der().ok()?.as_bytes().to_vec()),
        (false, "spki") => Some(p256::PublicKey::from_jwk_str(jwk).ok()?.to_public_key_der().ok()?.into_vec()),
        (false, "raw") => Some(p256::PublicKey::from_jwk_str(jwk).ok()?.to_encoded_point(false).as_bytes().to_vec()),
        (false, "pkcs8") => Some(p256::SecretKey::from_jwk_str(jwk).ok()?.to_pkcs8_der().ok()?.as_bytes().to_vec()),
        _ => None,
    }
}

fn jwk_gen(kty: &str, bits: usize, exponent: &[u8]) -> Option<String> {
    match kty {
        "RSA" => rsa_private_jwk(&RsaPrivateKey::new_with_exp(&mut thread_rng(), bits, &BigUint::from_bytes_be(exponent)).ok()?),
        "EC" => Some(p256::SecretKey::random(&mut thread_rng()).to_jwk_string().to_string()),
        _ => None,
    }
}

fn rsa_oaep<D: Hash>(encrypt: bool, jwk: &str, label: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    // ponytail: rsa 0.9 takes the OAEP label as a String, so non-UTF-8 labels fail (empty is the norm).
    let label = if label.is_empty() { None } else { Some(String::from_utf8(label.to_vec()).ok()?) };
    let pad = Oaep { digest: Box::new(D::new()), mgf_digest: Box::new(D::new()), label };
    if encrypt { rsa_public(jwk)?.encrypt(&mut thread_rng(), pad, data).ok() } else { rsa_private(jwk)?.decrypt(pad, data).ok() }
}

// WebCrypto ECDSA signatures are raw r||s (64 bytes for P-256), not DER.
fn ecdsa_sign<D: Hash>(jwk: &str, data: &[u8]) -> Option<Vec<u8>> {
    let key = SigningKey::from(p256::SecretKey::from_jwk_str(jwk).ok()?);
    let sig: Signature = key.sign_prehash(&<D as Digest>::digest(data)).ok()?;
    Some(sig.to_bytes().to_vec())
}

fn ecdsa_verify<D: Hash>(jwk: &str, data: &[u8], sig: &[u8]) -> Option<bool> {
    let key = VerifyingKey::from(p256::PublicKey::from_jwk_str(jwk).ok()?);
    let Ok(sig) = Signature::from_slice(sig) else { return Some(false) };
    Some(key.verify_prehash(&<D as Digest>::digest(data), &sig).is_ok())
}

pub fn install<'js>(ctx: &Ctx<'js>, g: &Object<'js>) -> rquickjs::Result<()> {
    g.set("__ws_digest", Function::new(ctx.clone(), |ctx: Ctx<'js>, alg: String, data: Bytes<'js>| {
        buf(&ctx, with_hash!(alg, digest(&bytes(&data))))
    })?)?;
    g.set("__ws_hmac", Function::new(ctx.clone(), |ctx: Ctx<'js>, alg: String, key: Bytes<'js>, data: Bytes<'js>| {
        buf(&ctx, with_hash!(alg, hmac(&bytes(&key), &bytes(&data))))
    })?)?;
    g.set("__ws_pbkdf2", Function::new(ctx.clone(), |ctx: Ctx<'js>, alg: String, pw: Bytes<'js>, salt: Bytes<'js>, rounds: u32, len: usize| {
        buf(&ctx, with_hash!(alg, pbkdf2(&bytes(&pw), &bytes(&salt), rounds, len)))
    })?)?;
    g.set("__ws_aes_gcm", Function::new(ctx.clone(), |ctx: Ctx<'js>, encrypt: bool, key: Bytes<'js>, iv: Bytes<'js>, aad: Bytes<'js>, data: Bytes<'js>| {
        buf(&ctx, aes_gcm(encrypt, &bytes(&key), &bytes(&iv), &bytes(&aad), &bytes(&data)))
    })?)?;
    g.set("__ws_rsa_oaep", Function::new(ctx.clone(), |ctx: Ctx<'js>, encrypt: bool, alg: String, jwk: String, label: Bytes<'js>, data: Bytes<'js>| {
        buf(&ctx, with_hash!(alg, rsa_oaep(encrypt, &jwk, &bytes(&label), &bytes(&data))))
    })?)?;
    g.set("__ws_ecdsa_sign", Function::new(ctx.clone(), |ctx: Ctx<'js>, alg: String, jwk: String, data: Bytes<'js>| {
        buf(&ctx, with_hash!(alg, ecdsa_sign(&jwk, &bytes(&data))))
    })?)?;
    g.set("__ws_ecdsa_verify", Function::new(ctx.clone(), |alg: String, jwk: String, data: Bytes<'js>, sig: Bytes<'js>| {
        with_hash!(alg, ecdsa_verify(&jwk, &bytes(&data), &bytes(&sig)))
    })?)?;
    g.set("__ws_jwk_from", Function::new(ctx.clone(), |kty: String, format: String, data: Bytes<'js>| {
        jwk_from(&kty, &format, &bytes(&data))
    })?)?;
    g.set("__ws_jwk_to", Function::new(ctx.clone(), |ctx: Ctx<'js>, format: String, jwk: String| {
        buf(&ctx, jwk_to(&format, &jwk))
    })?)?;
    g.set("__ws_jwk_gen", Function::new(ctx.clone(), |kty: String, bits: usize, exponent: Bytes<'js>| {
        jwk_gen(&kty, bits, &bytes(&exponent))
    })?)?;
    Ok(())
}
