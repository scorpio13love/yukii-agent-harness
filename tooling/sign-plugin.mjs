#!/usr/bin/env node
// 生成 Ed25519 密钥并给插件 manifest 签名；签名覆盖去掉 signature 的规范化 JSON。
import { generateKeyPairSync, sign, createPublicKey } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';

function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, canonical(value[key])]),
    );
  }
  return value;
}

const [command, ...args] = process.argv.slice(2);
if (command === 'keygen') {
  const { publicKey, privateKey } = generateKeyPairSync('ed25519');
  writeFileSync('plugin-private.pem', privateKey.export({ format: 'pem', type: 'pkcs8' }));
  const raw = createPublicKey(publicKey).export({ format: 'jwk' });
  const bytes = Buffer.from(raw.x, 'base64url');
  console.log(`private=plugin-private.pem\npublic_hex=${bytes.toString('hex')}`);
} else if (command === 'sign') {
  const [file, privateFile, publisher] = args;
  if (!file || !privateFile || !publisher) {
    console.error('用法: sign-plugin.mjs sign <manifest.json> <private.pem> <publisher>');
    process.exit(2);
  }
  const manifest = JSON.parse(readFileSync(file, 'utf8'));
  delete manifest.signature;
  manifest.publisher = publisher;
  const payload = Buffer.from(JSON.stringify(canonical(manifest)));
  const signature = sign(null, payload, readFileSync(privateFile));
  manifest.signature = signature.toString('base64url');
  writeFileSync(file, `${JSON.stringify(manifest, null, 2)}\n`);
  console.log(`signed ${file} publisher=${publisher}`);
} else {
  console.error('用法: sign-plugin.mjs keygen | sign <manifest> <private.pem> <publisher>');
  process.exit(2);
}
