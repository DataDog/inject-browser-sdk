const path = require('node:path');
const createRumInjector = require('./rum_injector');

async function main() {
  const wasmPath = path.resolve(
    __dirname,
    '../../target/wasm32-unknown-unknown/release/inject_browser_sdk_ffi.wasm',
  );
  console.log(`Loading WASM from ${wasmPath}...`);

  const rumFactory = await createRumInjector(wasmPath);

  const rumMiddleware = rumFactory({
    majorVersion: 5,
    rum: {
      applicationId: 'my-app-id',
      clientToken: 'my-client-token',
      site: 'datadoghq.com',
    },
  });

  const htmlInput = '<head><title>Test</title></head><body>Hello</body>';

  // Mock response object to capture injected output
  const chunks = [];
  const res = {
    getHeader(name) {
      if (name.toLowerCase() === 'content-type') return 'text/html';
      return undefined;
    },
    setHeader() {},
    removeHeader() {},
    write(chunk) {
      chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
      return true;
    },
    end(chunk, encoding, callback) {
      if (typeof chunk === 'function') {
        callback = chunk;
      } else if (chunk) {
        chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
      }
      if (callback) callback();
    },
  };

  rumMiddleware({}, res, () => {
    res.write(htmlInput);
    res.end();
  });

  const output = Buffer.concat(chunks).toString('utf-8');
  console.log('Injected output:\n', output);
}

main().catch((e) => {
  console.error('Failed:', e);
  process.exit(1);
});
