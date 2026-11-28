package main

import (
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"crypto/tls"
	"crypto/x509"
	"encoding/base32"
	"errors"
	"fmt"
	"math/big"
	"strings"
	"time"
)

const secretSize = 24
const pinSize = 20

var encoding = base32.StdEncoding.WithPadding(base32.NoPadding)
var errCredential = errors.New("invalid sharing token")

type identity struct {
	Seed   []byte `json:"seed"`
	Secret []byte `json:"secret"`
}

func randomBytes(n int) ([]byte, error) {
	b := make([]byte, n)
	_, err := rand.Read(b)
	return b, err
}

func newIdentity() (identity, error) {
	seed, err := randomBytes(ed25519.SeedSize)
	if err != nil {
		return identity{}, err
	}
	secret, err := randomBytes(secretSize)
	return identity{seed, secret}, err
}

func fingerprint(key ed25519.PublicKey) []byte {
	sum := sha256.Sum256(key)
	return sum[:pinSize]
}

func (i identity) token() string {
	key := ed25519.NewKeyFromSeed(i.Seed)
	return "DIM1-" + encoding.EncodeToString(fingerprint(key.Public().(ed25519.PublicKey))) + "-" + encoding.EncodeToString(i.Secret)
}

type credential struct{ pin, secret []byte }

func parseCredential(text string) (credential, error) {
	parts := strings.Split(strings.ToUpper(strings.TrimSpace(text)), "-")
	if len(parts) != 3 || parts[0] != "DIM1" {
		return credential{}, errCredential
	}
	pin, e1 := encoding.DecodeString(parts[1])
	secret, e2 := encoding.DecodeString(parts[2])
	if e1 != nil || e2 != nil || len(pin) != pinSize || len(secret) != secretSize {
		return credential{}, errCredential
	}
	return credential{pin, secret}, nil
}

func (i identity) certificate() (tls.Certificate, error) {
	if len(i.Seed) != ed25519.SeedSize || len(i.Secret) != secretSize {
		return tls.Certificate{}, errCredential
	}
	key := ed25519.NewKeyFromSeed(i.Seed)
	pub := key.Public().(ed25519.PublicKey)
	now := time.Now()
	cert := &x509.Certificate{SerialNumber: new(big.Int).SetBytes(fingerprint(pub)), NotBefore: now.Add(-time.Hour), NotAfter: now.AddDate(1, 0, 0), KeyUsage: x509.KeyUsageDigitalSignature, ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth}}
	der, err := x509.CreateCertificate(rand.Reader, cert, cert, pub, key)
	return tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key}, err
}

func (c credential) verify(state tls.ConnectionState) error {
	if len(state.PeerCertificates) != 1 {
		return errors.New("unexpected sharing certificate chain")
	}
	pub, ok := state.PeerCertificates[0].PublicKey.(ed25519.PublicKey)
	if !ok || subtle.ConstantTimeCompare(c.pin, fingerprint(pub)) != 1 {
		return fmt.Errorf("sharing computer identity does not match the token")
	}
	return nil
}
