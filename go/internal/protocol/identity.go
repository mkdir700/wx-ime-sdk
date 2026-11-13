// Package protocol implements the control wire format shared with the Rust SDK.
package protocol

/*
#cgo pkg-config: openssl
#cgo CFLAGS: -Wno-deprecated-declarations
#include <openssl/ec.h>
#include <openssl/pem.h>
#include <openssl/evp.h>
#include <openssl/buffer.h>
#include <openssl/crypto.h>
#include <stdlib.h>

static EVP_PKEY *wx_read(const void *pem, int n) {
 BIO *b = BIO_new_mem_buf(pem, n);
 if (!b) return NULL;
 EVP_PKEY *key = PEM_read_bio_PrivateKey(b, NULL, NULL, NULL);
 BIO_free(b);
 if (!key) return NULL;
 EC_KEY *ec = EVP_PKEY_get1_EC_KEY(key);
 int ok = ec && EC_GROUP_get_curve_name(EC_KEY_get0_group(ec)) == NID_secp128r1 && EC_KEY_check_key(ec) == 1;
 EC_KEY_free(ec);
 if (!ok) { EVP_PKEY_free(key); return NULL; }
 return key;
}
static int wx_generate(unsigned char **out) {
 EC_KEY *ec = EC_KEY_new_by_curve_name(NID_secp128r1);
 EVP_PKEY *key = EVP_PKEY_new();
 BIO *b = BIO_new(BIO_s_mem());
 int n = 0;
 if (!ec || !key || !b || EC_KEY_generate_key(ec) != 1 || EVP_PKEY_set1_EC_KEY(key, ec) != 1 || PEM_write_bio_PKCS8PrivateKey(b, key, NULL, NULL, 0, NULL, NULL) != 1) goto done;
 BUF_MEM *buf;
 BIO_get_mem_ptr(b, &buf);
 *out = OPENSSL_memdup(buf->data, buf->length);
 if (*out) n = (int)buf->length;
 done: BIO_free(b); EVP_PKEY_free(key); EC_KEY_free(ec); return n;
}
static int wx_public(const void *pem, int n, unsigned char *out) {
 EVP_PKEY *key = wx_read(pem,n);
 if (!key) return 0;
 EC_KEY *ec = EVP_PKEY_get1_EC_KEY(key);
 int result = ec ? (int)EC_POINT_point2oct(EC_KEY_get0_group(ec), EC_KEY_get0_public_key(ec), POINT_CONVERSION_UNCOMPRESSED, out, 33, NULL) : 0;
 EC_KEY_free(ec); EVP_PKEY_free(key); return result == 33;
}
static int wx_agree(const void *pem, int n, const unsigned char *peer, unsigned char *out) {
 EVP_PKEY *key = wx_read(pem,n);
 if (!key) return 0;
 EC_KEY *ec = EVP_PKEY_get1_EC_KEY(key);
 EC_POINT *point = ec ? EC_POINT_new(EC_KEY_get0_group(ec)) : NULL;
 int ok = ec && point && EC_POINT_oct2point(EC_KEY_get0_group(ec), point, peer, 33, NULL) == 1 && EC_POINT_is_on_curve(EC_KEY_get0_group(ec),point,NULL) == 1 && EC_POINT_is_at_infinity(EC_KEY_get0_group(ec),point) == 0;
 int result = ok ? ECDH_compute_key(out,16,point,ec,NULL) : 0;
 EC_POINT_free(point); EC_KEY_free(ec); EVP_PKEY_free(key); return result == 16;
}
static void wx_clear(unsigned char *p, int n) { OPENSSL_clear_free(p,n); }
*/
import "C"

import (
	"encoding/hex"
	"errors"
	"strings"
	"unsafe"
)

func GenerateIdentity() ([]byte, error) {
	var out *C.uchar
	n := C.wx_generate(&out)
	if n == 0 {
		return nil, errors.New("wxime: OpenSSL identity generation failed")
	}
	defer C.wx_clear(out, n)
	return C.GoBytes(unsafe.Pointer(out), n), nil
}
func PublicKey(pem []byte) (string, error) {
	if len(pem) == 0 {
		return "", errors.New("wxime: empty identity")
	}
	out := make([]byte, 33)
	if C.wx_public(unsafe.Pointer(&pem[0]), C.int(len(pem)), (*C.uchar)(unsafe.Pointer(&out[0]))) != 1 {
		return "", errors.New("wxime: invalid secp128r1 identity")
	}
	return strings.ToUpper(hex.EncodeToString(out)), nil
}
func Agree(pem []byte, public string) ([]byte, error) {
	peer, err := hex.DecodeString(public)
	if err != nil || len(peer) != 33 || peer[0] != 4 || len(pem) == 0 {
		return nil, errors.New("wxime: invalid peer public key")
	}
	secret := make([]byte, 16)
	defer clear(secret)
	if C.wx_agree(unsafe.Pointer(&pem[0]), C.int(len(pem)), (*C.uchar)(unsafe.Pointer(&peer[0])), (*C.uchar)(unsafe.Pointer(&secret[0]))) != 1 {
		return nil, errors.New("wxime: ECDH agreement failed")
	}
	return []byte(strings.ToUpper(hex.EncodeToString(secret))), nil
}
