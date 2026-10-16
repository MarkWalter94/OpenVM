# Generates a self-signed Secure Boot platform key certificate (DER) and
# discards the private key: nobody can later sign KEK updates with it.
# usage: python -I gen-pk.py <out.der> [common name]
import datetime, sys
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import NameOID

out = sys.argv[1]
cn = sys.argv[2] if len(sys.argv) > 2 else 'LightVM Platform Key'
key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, cn)])
now = datetime.datetime.now(datetime.timezone.utc)
cert = (x509.CertificateBuilder().subject_name(name).issuer_name(name)
        .public_key(key.public_key()).serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(days=1))
        .not_valid_after(now + datetime.timedelta(days=365 * 30))
        .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
        .sign(key, hashes.SHA256()))
with open(out, 'wb') as f:
    f.write(cert.public_bytes(serialization.Encoding.DER))
del key
