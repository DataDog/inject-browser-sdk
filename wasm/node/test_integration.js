const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const WASM_PATH = path.resolve(
  __dirname,
  '../../target/wasm32-unknown-unknown/release/inject_browser_sdk_ffi.wasm',
);

const VALID_CONFIG = {
  majorVersion: 5,
  rum: {
    applicationId: '00000000-0000-0000-0000-000000000000',
    clientToken: 'pub00000000000000000000000000000000',
    site: 'datadoghq.com',
  },
};

// ─── Helpers ────────────────────────────────────────────────────────────────

async function loadWasm() {
  const wasmBuffer = fs.readFileSync(WASM_PATH);
  const { instance } = await WebAssembly.instantiate(wasmBuffer);
  return instance.exports;
}

function writeString(exports, str) {
  const encoder = new TextEncoder();
  const bytes = encoder.encode(str);
  const ptr = exports.malloc(bytes.length + 1);
  const buf = new Uint8Array(exports.memory.buffer, ptr, bytes.length + 1);
  buf.set(bytes);
  buf[bytes.length] = 0;
  return ptr;
}

// Snippet layout (repr(C), wasm32):
//   error_code: u8   @ 0
//   (padding 3 bytes)
//   error_message: u32 @ 4
//   length: u32       @ 8
function readSnippet(exports, ptr) {
  const view = new DataView(exports.memory.buffer);
  const errorCode = view.getUint8(ptr);
  let errorMessage = null;
  if (errorCode !== 0) {
    const msgPtr = view.getUint32(ptr + 4, true);
    const mem = new Uint8Array(exports.memory.buffer);
    const maxEnd = Math.min(msgPtr + 65536, mem.length);
    let end = msgPtr;
    while (end < maxEnd && mem[end] !== 0) end++;
    errorMessage = new TextDecoder().decode(mem.slice(msgPtr, end));
  }
  const length = view.getUint32(ptr + 8, true);
  return { errorCode, errorMessage, length };
}

// InjectorResult layout (repr(C), wasm32):
//   slices_length: u32 @ 0
//   slices: u32        @ 4
//   injected: bool     @ 8
// BytesSlice layout (repr(C), wasm32), stride 12:
//   length: u32            @ 0
//   start: u32             @ 4
//   from_incoming_chunk: bool @ 8
function readInjectorResult(exports, ptr) {
  const view = new DataView(exports.memory.buffer);
  const slicesLength = view.getUint32(ptr, true);
  const slicesPtr = view.getUint32(ptr + 4, true);
  const injected = view.getUint8(ptr + 8) !== 0;

  const slices = [];
  for (let i = 0; i < slicesLength; i++) {
    const off = slicesPtr + i * 12;
    const len = view.getUint32(off, true);
    const start = view.getUint32(off + 4, true);
    slices.push(new Uint8Array(exports.memory.buffer, start, len).slice());
  }
  return { slices, injected };
}

function concatSlices(slices) {
  const totalLen = slices.reduce((s, b) => s + b.length, 0);
  const out = new Uint8Array(totalLen);
  let offset = 0;
  for (const s of slices) {
    out.set(s, offset);
    offset += s.length;
  }
  return new TextDecoder().decode(out);
}

// Inject a full HTML string via injector_write + injector_end in one shot.
function injectFull(exports, snippetPtr, html) {
  const injector = exports.injector_create(snippetPtr);
  const resultPtr = exports.malloc(12);

  const encoder = new TextEncoder();
  const htmlBytes = encoder.encode(html);
  const chunkPtr = exports.malloc(htmlBytes.length);
  new Uint8Array(exports.memory.buffer, chunkPtr, htmlBytes.length).set(htmlBytes);

  exports.injector_write(resultPtr, injector, chunkPtr, htmlBytes.length);
  const writeResult = readInjectorResult(exports, resultPtr);

  exports.injector_end(resultPtr, injector);
  const endResult = readInjectorResult(exports, resultPtr);

  const allSlices = [...writeResult.slices, ...endResult.slices];
  const output = concatSlices(allSlices);

  exports.free(chunkPtr);
  exports.free(resultPtr);
  exports.injector_cleanup(injector);

  return { output, injected: writeResult.injected || endResult.injected };
}

// ─── Low-level WASM API tests ───────────────────────────────────────────────

describe('Low-level WASM API', async () => {
  let exports;

  it('loads the WASM module', async () => {
    exports = await loadWasm();
    assert.ok(exports.snippet_create_from_json, 'snippet_create_from_json should be exported');
    assert.ok(exports.injector_create, 'injector_create should be exported');
    assert.ok(exports.malloc, 'malloc should be exported');
  });

  it('creates a snippet with valid config (error_code == 0, length > 0)', async () => {
    if (!exports) exports = await loadWasm();
    const configPtr = writeString(exports, JSON.stringify(VALID_CONFIG));
    const snippetPtr = exports.snippet_create_from_json(configPtr);
    const snippet = readSnippet(exports, snippetPtr);

    assert.equal(snippet.errorCode, 0, 'error_code should be 0');
    assert.ok(snippet.length > 0, `snippet length should be > 0, got ${snippet.length}`);

    exports.snippet_cleanup(snippetPtr);
    exports.free(configPtr);
  });

  it('fails with invalid JSON (error_code == 1)', async () => {
    if (!exports) exports = await loadWasm();
    const configPtr = writeString(exports, '{invalid json');
    const snippetPtr = exports.snippet_create_from_json(configPtr);
    const snippet = readSnippet(exports, snippetPtr);

    assert.equal(snippet.errorCode, 1, 'error_code should be 1 for invalid JSON');
    assert.ok(snippet.errorMessage, 'error_message should be set');
    assert.match(snippet.errorMessage, /JSON error/i);

    exports.snippet_cleanup(snippetPtr);
    exports.free(configPtr);
  });

  it('fails with unsupported major version (error_code == 2)', async () => {
    if (!exports) exports = await loadWasm();
    const config = {
      majorVersion: 1,
      rum: {
        applicationId: 'foo',
        clientToken: 'bar',
        site: 'datadoghq.com',
      },
    };
    const configPtr = writeString(exports, JSON.stringify(config));
    const snippetPtr = exports.snippet_create_from_json(configPtr);
    const snippet = readSnippet(exports, snippetPtr);

    assert.equal(snippet.errorCode, 2, 'error_code should be 2 for unsupported version');
    assert.ok(snippet.errorMessage, 'error_message should be set');
    assert.match(snippet.errorMessage, /not supported/i);

    exports.snippet_cleanup(snippetPtr);
    exports.free(configPtr);
  });

  it('injects snippet before </head> tag', async () => {
    if (!exports) exports = await loadWasm();
    const configPtr = writeString(exports, JSON.stringify(VALID_CONFIG));
    const snippetPtr = exports.snippet_create_from_json(configPtr);

    const html = '<html><head><title>Test</title></head><body>Hello</body></html>';
    const { output, injected } = injectFull(exports, snippetPtr, html);

    assert.ok(injected, 'injected should be true');
    assert.ok(output.includes('datadog-rum'), `output should contain datadog-rum script, got: ${output.substring(0, 200)}`);
    // The snippet should appear before </head>
    const snippetIndex = output.indexOf('datadog-rum');
    const headCloseIndex = output.indexOf('</head>');
    assert.ok(snippetIndex < headCloseIndex, 'snippet should appear before </head>');
    // Original content should still be there
    assert.ok(output.includes('<body>Hello</body>'), 'body content should be preserved');

    exports.snippet_cleanup(snippetPtr);
    exports.free(configPtr);
  });

  it('handles HTML without </head> (pads with spaces)', async () => {
    if (!exports) exports = await loadWasm();
    const configPtr = writeString(exports, JSON.stringify(VALID_CONFIG));
    const snippetPtr = exports.snippet_create_from_json(configPtr);
    const snippetInfo = readSnippet(exports, snippetPtr);

    const html = '<html><body>No head tag here</body></html>';
    const { output, injected } = injectFull(exports, snippetPtr, html);

    // When there's no </head>, injection doesn't happen, but end() pads with spaces
    assert.ok(!injected, 'injected should be false when no </head> found');
    // The output should contain the original content
    assert.ok(output.includes('No head tag here'), 'original content should be preserved');
    // End should pad with spaces equal to snippet length
    assert.equal(output.length, html.length + snippetInfo.length,
      `output length should be original + snippet length (padding)`);

    exports.snippet_cleanup(snippetPtr);
    exports.free(configPtr);
  });

  it('handles streaming chunks (split across </head>)', async () => {
    if (!exports) exports = await loadWasm();
    const configPtr = writeString(exports, JSON.stringify(VALID_CONFIG));
    const snippetPtr = exports.snippet_create_from_json(configPtr);

    const html = '<html><head><title>Test</title></head><body>Hello</body></html>';
    // Split in the middle of </head> -> "</he" | "ad>..."
    const splitPoint = html.indexOf('</head>') + 3; // "</he"
    const chunk1 = html.substring(0, splitPoint);
    const chunk2 = html.substring(splitPoint);

    const injector = exports.injector_create(snippetPtr);
    const resultPtr = exports.malloc(12);
    const encoder = new TextEncoder();

    // Write chunk 1
    const bytes1 = encoder.encode(chunk1);
    const ptr1 = exports.malloc(bytes1.length);
    new Uint8Array(exports.memory.buffer, ptr1, bytes1.length).set(bytes1);
    exports.injector_write(resultPtr, injector, ptr1, bytes1.length);
    const r1 = readInjectorResult(exports, resultPtr);
    exports.free(ptr1);

    // Write chunk 2
    const bytes2 = encoder.encode(chunk2);
    const ptr2 = exports.malloc(bytes2.length);
    new Uint8Array(exports.memory.buffer, ptr2, bytes2.length).set(bytes2);
    exports.injector_write(resultPtr, injector, ptr2, bytes2.length);
    const r2 = readInjectorResult(exports, resultPtr);
    exports.free(ptr2);

    // End
    exports.injector_end(resultPtr, injector);
    const r3 = readInjectorResult(exports, resultPtr);

    const allSlices = [...r1.slices, ...r2.slices, ...r3.slices];
    const output = concatSlices(allSlices);

    assert.ok(r1.injected || r2.injected, 'injection should happen in one of the chunks');
    assert.ok(output.includes('datadog-rum'), 'output should contain datadog-rum script');
    assert.ok(output.includes('<body>Hello</body>'), 'body content should be preserved');

    exports.free(resultPtr);
    exports.injector_cleanup(injector);
    exports.snippet_cleanup(snippetPtr);
    exports.free(configPtr);
  });
});

// ─── rum_injector.js middleware tests ────────────────────────────────────────

const createRumInjector = require('./rum_injector');

// Create a mock Express-style response object
function createMockRes() {
  const chunks = [];
  let headers = {};
  let ended = false;

  const res = {
    _headers: headers,
    _chunks: chunks,
    _ended: false,
    setHeader(name, value) { headers[name.toLowerCase()] = value; },
    getHeader(name) { return headers[name.toLowerCase()]; },
    removeHeader(name) { delete headers[name.toLowerCase()]; },
    write(chunk, encoding, callback) {
      if (typeof encoding === 'function') { callback = encoding; encoding = null; }
      if (typeof chunk === 'string') chunk = Buffer.from(chunk, encoding || 'utf-8');
      chunks.push(chunk);
      if (callback) callback();
      return true;
    },
    end(chunk, encoding, callback) {
      if (typeof chunk === 'function') { callback = chunk; chunk = null; encoding = null; }
      else if (typeof encoding === 'function') { callback = encoding; encoding = null; }
      if (chunk) {
        if (typeof chunk === 'string') chunk = Buffer.from(chunk, encoding || 'utf-8');
        chunks.push(chunk);
      }
      res._ended = true;
      if (callback) callback();
    },
    getBody() {
      return Buffer.concat(chunks).toString('utf-8');
    },
  };
  return res;
}

describe('rum_injector.js middleware', () => {
  let rumFactory;

  it('loads the middleware factory', async () => {
    rumFactory = await createRumInjector(WASM_PATH);
    assert.ok(typeof rumFactory === 'function', 'createRumInjector should return a factory function');
  });

  it('injects into text/html responses', async () => {
    if (!rumFactory) rumFactory = await createRumInjector(WASM_PATH);
    const middleware = rumFactory(VALID_CONFIG);

    const req = {};
    const res = createMockRes();

    await new Promise((resolve) => {
      middleware(req, res, () => {
        res.setHeader('Content-Type', 'text/html; charset=utf-8');
        res.end('<html><head><title>Test</title></head><body>Hi</body></html>');
        resolve();
      });
    });

    const body = res.getBody();
    assert.ok(res._ended, 'response should be ended');
    assert.ok(body.includes('datadog-rum'), `body should contain datadog-rum script, got: ${body.substring(0, 200)}`);
    assert.ok(body.includes('<body>Hi</body>'), 'original body content should be preserved');
  });

  it('skips non-HTML responses (application/json)', async () => {
    if (!rumFactory) rumFactory = await createRumInjector(WASM_PATH);
    const middleware = rumFactory(VALID_CONFIG);

    const req = {};
    const res = createMockRes();
    const jsonPayload = JSON.stringify({ status: 'ok' });

    await new Promise((resolve) => {
      middleware(req, res, () => {
        res.setHeader('Content-Type', 'application/json');
        res.end(jsonPayload);
        resolve();
      });
    });

    const body = res.getBody();
    assert.equal(body, jsonPayload, 'JSON body should pass through unchanged');
    assert.ok(!body.includes('datadog-rum'), 'JSON response should not be injected');
  });

  it('handles chunked/streamed HTML responses', async () => {
    if (!rumFactory) rumFactory = await createRumInjector(WASM_PATH);
    const middleware = rumFactory(VALID_CONFIG);

    const req = {};
    const res = createMockRes();
    const html = '<html><head><title>Chunked</title></head><body>Streaming!</body></html>';
    // Split into 3 chunks
    const chunk1 = html.substring(0, 20);
    const chunk2 = html.substring(20, 50);
    const chunk3 = html.substring(50);

    await new Promise((resolve) => {
      middleware(req, res, () => {
        res.setHeader('Content-Type', 'text/html');
        res.write(chunk1);
        res.write(chunk2);
        res.end(chunk3);
        resolve();
      });
    });

    const body = res.getBody();
    assert.ok(res._ended, 'response should be ended');
    assert.ok(body.includes('datadog-rum'), 'chunked body should contain datadog-rum script');
    assert.ok(body.includes('Streaming!'), 'original content should be preserved');
  });

  it('injected output contains datadog-rum.js and config values', async () => {
    if (!rumFactory) rumFactory = await createRumInjector(WASM_PATH);
    const config = {
      majorVersion: 5,
      rum: {
        applicationId: 'test-app-id-12345',
        clientToken: 'pub-test-token-67890',
        site: 'datadoghq.eu',
        service: 'my-test-service',
        env: 'staging',
      },
    };
    const middleware = rumFactory(config);

    const req = {};
    const res = createMockRes();

    await new Promise((resolve) => {
      middleware(req, res, () => {
        res.setHeader('Content-Type', 'text/html');
        res.end('<html><head></head><body></body></html>');
        resolve();
      });
    });

    const body = res.getBody();
    assert.ok(body.includes('datadog-rum'), 'should contain datadog-rum script reference');
    assert.ok(body.includes('test-app-id-12345'), 'should contain applicationId');
    assert.ok(body.includes('pub-test-token-67890'), 'should contain clientToken');
    assert.ok(body.includes('datadoghq.eu'), 'should contain site');
    assert.ok(body.includes('my-test-service'), 'should contain service name');
    assert.ok(body.includes('staging'), 'should contain env');
  });
});
