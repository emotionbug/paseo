import 'dart:convert';
import 'dart:typed_data';

import 'package:cryptography/cryptography.dart';
import 'package:cryptography/dart.dart';
import 'package:pinenacl/tweetnacl.dart' as nacl;

/// Canonical padded RFC 4648, not Dart's permissive URL/base64 normalizer.
Uint8List decodeRelayBase64(String text) {
  if (text.length % 4 != 0 ||
      !RegExp(
        r'^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$',
      ).hasMatch(text)) {
    throw const FormatException('Invalid relay base64 encoding');
  }
  final bytes = base64.decode(text);
  if (base64.encode(bytes) != text) {
    throw const FormatException('Noncanonical relay base64 encoding');
  }
  return bytes;
}

Uint8List importRelayPublicKey(String text) {
  final bytes = decodeRelayBase64(text);
  if (bytes.length != 32) {
    throw const FormatException('Relay public key must be 32 bytes');
  }
  return bytes;
}

Uint8List _concat(List<int> first, List<int> second) {
  return Uint8List(first.length + second.length)
    ..setAll(0, first)
    ..setAll(first.length, second);
}

Future<Uint8List> _dh(SimpleKeyPair key, Uint8List remote) async {
  if (remote.length != 32) {
    throw const FormatException('Invalid X25519 public key length');
  }
  final shared = await X25519().sharedSecretKey(
    keyPair: key,
    remotePublicKey: SimplePublicKey(remote, type: KeyPairType.x25519),
  );
  try {
    final bytes = Uint8List.fromList(await shared.extractBytes());
    var accumulator = 0;
    for (final byte in bytes) {
      accumulator |= byte;
    }
    if (accumulator == 0) {
      bytes.fillRange(0, bytes.length, 0);
      throw const FormatException('Invalid X25519 public key');
    }
    return bytes;
  } finally {
    shared.destroy();
  }
}

/// Synchronous ChaChaPoly prevents concurrent sends from reusing a nonce or
/// interleaving chunks. Receiving advances only after authentication succeeds.
class RelayNoiseCipher {
  RelayNoiseCipher(Uint8List key)
    : _key = SecretKeyData(
        Uint8List.fromList(key),
        overwriteWhenDestroyed: true,
      );

  static const _algorithm = DartChacha20.poly1305Aead();
  static final _maximumNonce = (BigInt.one << 64) - BigInt.one;
  SecretKeyData? _key;
  BigInt _nonce = BigInt.zero;

  Uint8List _encodedNonce() {
    if (_key == null || _nonce >= _maximumNonce) {
      throw StateError('Noise nonce exhausted or cipher closed');
    }
    final bytes = Uint8List(12);
    var n = _nonce;
    for (var index = 4; index < 12; index++) {
      bytes[index] = (n & BigInt.from(255)).toInt();
      n >>= 8;
    }
    return bytes;
  }

  Uint8List encrypt(Uint8List plaintext, {List<int> aad = const []}) {
    if (plaintext.length + 16 > 65535) {
      throw const FormatException('Noise message too large');
    }
    final nonce = _encodedNonce();
    _nonce += BigInt.one; // Reserve before calling any cryptographic operation.
    final box = _algorithm.encryptSync(
      plaintext,
      secretKey: _key!,
      nonce: nonce,
      aad: aad,
    );
    return _concat(box.cipherText, box.mac.bytes);
  }

  Uint8List decrypt(Uint8List ciphertext, {List<int> aad = const []}) {
    if (ciphertext.length < 16 || ciphertext.length > 65535) {
      throw const FormatException('Invalid Noise ciphertext length');
    }
    final nonce = _encodedNonce();
    final tagOffset = ciphertext.length - 16;
    final plaintext = _algorithm.decryptSync(
      SecretBox(
        Uint8List.sublistView(ciphertext, 0, tagOffset),
        nonce: nonce,
        mac: Mac(Uint8List.sublistView(ciphertext, tagOffset)),
      ),
      secretKey: _key!,
      aad: aad,
    );
    _nonce += BigInt.one;
    return plaintext is Uint8List ? plaintext : Uint8List.fromList(plaintext);
  }

  void clear() {
    _key?.destroy();
    _key = null;
    _nonce = _maximumNonce;
  }
}

class _NoiseSymmetricState {
  _NoiseSymmetricState() {
    final name = utf8.encode('Noise_NK_25519_ChaChaPoly_SHA256');
    hash = name.length <= 32
        ? (Uint8List(32)..setAll(0, name))
        : Uint8List.fromList(_sha256.hashSync(name).bytes);
    _chainingKey = Uint8List.fromList(hash);
  }

  static const _sha256 = DartSha256();
  static const _hmac = DartHmac(DartSha256());
  late Uint8List hash;
  late Uint8List _chainingKey;
  RelayNoiseCipher? cipher;

  static Uint8List _mac(Uint8List key, List<int> input) {
    final secret = SecretKeyData(
      Uint8List.fromList(key),
      overwriteWhenDestroyed: true,
    );
    try {
      return Uint8List.fromList(
        _hmac
            .calculateMacSync(input, secretKeyData: secret, nonce: const [])
            .bytes,
      );
    } finally {
      secret.destroy();
    }
  }

  (Uint8List, Uint8List) _hkdf(Uint8List input) {
    final temporary = _mac(_chainingKey, input);
    try {
      final first = _mac(temporary, const [1]);
      final second = _mac(temporary, _concat(first, const [2]));
      return (first, second);
    } finally {
      temporary.fillRange(0, temporary.length, 0);
    }
  }

  void mixHash(Uint8List bytes) {
    hash = Uint8List.fromList(_sha256.hashSync(_concat(hash, bytes)).bytes);
  }

  void mixKey(Uint8List shared) {
    final (next, key) = _hkdf(shared);
    _chainingKey.fillRange(0, _chainingKey.length, 0);
    _chainingKey = next;
    cipher?.clear();
    cipher = RelayNoiseCipher(key);
    key.fillRange(0, key.length, 0);
  }

  (RelayNoiseCipher, RelayNoiseCipher) split() {
    final (send, receive) = _hkdf(Uint8List(0));
    try {
      return (RelayNoiseCipher(send), RelayNoiseCipher(receive));
    } finally {
      send.fillRange(0, send.length, 0);
      receive.fillRange(0, receive.length, 0);
      clear();
    }
  }

  void clear() {
    cipher?.clear();
    cipher = null;
    _chainingKey.fillRange(0, _chainingKey.length, 0);
    hash.fillRange(0, hash.length, 0);
  }
}

/// Noise revision 34 NK initiator: <- s; -> e, es; <- e, ee.
class RelayNoiseInitiator {
  RelayNoiseInitiator._(this._ephemeral, this._symmetric, this.helloMessage);

  SimpleKeyPair? _ephemeral;
  final _NoiseSymmetricState _symmetric;
  final Uint8List helloMessage;
  bool _consumed = false;

  static Future<RelayNoiseInitiator> create(Uint8List remoteStatic) async {
    final state = _NoiseSymmetricState();
    SimpleKeyPair? ephemeral;
    try {
      state.mixHash(Uint8List.fromList(utf8.encode('paseo-relay-e2ee/1')));
      state.mixHash(remoteStatic);
      ephemeral = await X25519().newKeyPair();
      final public = Uint8List.fromList(
        (await ephemeral.extractPublicKey()).bytes,
      );
      state.mixHash(public);
      final shared = await _dh(ephemeral, remoteStatic);
      try {
        state.mixKey(shared);
      } finally {
        shared.fillRange(0, shared.length, 0);
      }
      final payload = state.cipher!.encrypt(
        Uint8List.fromList([1, 1]),
        aad: state.hash,
      );
      state.mixHash(payload);
      return RelayNoiseInitiator._(ephemeral, state, _concat(public, payload));
    } catch (_) {
      ephemeral?.destroy();
      state.clear();
      rethrow;
    }
  }

  Future<
    ({RelayNoiseCipher send, RelayNoiseCipher receive, bool binaryCiphertext})
  >
  finish(Uint8List message) async {
    if (_consumed || _ephemeral == null) {
      throw StateError('Noise handshake already consumed');
    }
    _consumed = true;
    if (message.length < 48 || message.length > 65535) {
      throw const FormatException('Invalid Noise handshake message length');
    }
    final remote = Uint8List.sublistView(message, 0, 32);
    _symmetric.mixHash(remote);
    final shared = await _dh(_ephemeral!, remote);
    try {
      if (_ephemeral == null) throw StateError('Noise handshake closed');
      _symmetric.mixKey(shared);
      final ciphertext = Uint8List.sublistView(message, 32);
      final payload = _symmetric.cipher!.decrypt(
        ciphertext,
        aad: _symmetric.hash,
      );
      _symmetric.mixHash(ciphertext);
      if (payload.length != 2 || payload[0] != 1 || (payload[1] & ~1) != 0) {
        throw const FormatException('Invalid protected relay parameters');
      }
      final (send, receive) = _symmetric.split();
      return (send: send, receive: receive, binaryCiphertext: payload[1] == 1);
    } finally {
      shared.fillRange(0, shared.length, 0);
      clear();
    }
  }

  void clear() {
    _ephemeral?.destroy();
    _ephemeral = null;
    _symmetric.clear();
  }
}

/// NaCl box.before (X25519 then HSalsa20), not raw X25519 as a secretbox key.
class RelayLegacyBox {
  RelayLegacyBox._(this._key, this.publicKey);

  Uint8List? _key;
  final Uint8List publicKey;

  static Future<RelayLegacyBox> create(Uint8List remote) async {
    // Separate key pair from Noise, even though both use X25519.
    final ephemeral = await X25519().newKeyPair();
    final key = Uint8List(32);
    try {
      final public = Uint8List.fromList(
        (await ephemeral.extractPublicKey()).bytes,
      );
      final shared = await _dh(ephemeral, remote);
      try {
        // Exactly nacl.box.before: HSalsa20(zero16, X25519, sigma).
        nacl.TweetNaCl.crypto_core_hsalsa20(
          key,
          Uint8List(16),
          shared,
          Uint8List.fromList(ascii.encode('expand 32-byte k')),
        );
        return RelayLegacyBox._(key, public);
      } finally {
        shared.fillRange(0, shared.length, 0);
      }
    } catch (_) {
      key.fillRange(0, key.length, 0);
      rethrow;
    } finally {
      ephemeral.destroy();
    }
  }

  Uint8List encrypt(Uint8List plaintext) {
    final key = _key;
    if (key == null) throw StateError('Legacy relay cipher closed');
    final nonce = nacl.TweetNaCl.randombytes(24);
    // NaCl's low-level API requires 32 leading plaintext zeros and returns
    // tag || ciphertext. Using it avoids immutable secret copies in Box.
    final padded = Uint8List(plaintext.length + 32)..setAll(32, plaintext);
    final scratch = Uint8List(padded.length);
    try {
      final ciphertext = nacl.TweetNaCl.crypto_box_afternm(
        scratch,
        padded,
        padded.length,
        nonce,
        key,
      );
      return _concat(nonce, ciphertext);
    } finally {
      padded.fillRange(0, padded.length, 0);
      scratch.fillRange(0, scratch.length, 0);
    }
  }

  Uint8List decrypt(Uint8List record) {
    final key = _key;
    if (key == null) throw StateError('Legacy relay cipher closed');
    if (record.length < 40) {
      throw const FormatException('Legacy ciphertext too short');
    }
    final nonce = Uint8List.sublistView(record, 0, 24);
    final padded = Uint8List(record.length - 24 + 16);
    padded.setRange(16, padded.length, record, 24);
    final scratch = Uint8List(padded.length);
    try {
      return nacl.TweetNaCl.crypto_box_open_afternm(
        scratch,
        padded,
        padded.length,
        nonce,
        key,
      );
    } finally {
      scratch.fillRange(0, scratch.length, 0);
    }
  }

  void clear() {
    _key?.fillRange(0, _key!.length, 0);
    _key = null;
  }
}
