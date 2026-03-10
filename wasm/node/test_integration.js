const { describe, it, before } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');

const createRumInjector = require('./rum_injector');

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

function createMockRes() {
  const chunks = [];
  let headers = {};

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

// ─── Middleware tests ────────────────────────────────────────────────────────

describe('rum_injector.js middleware', () => {
  let rumFactory;

  before(async () => {
    rumFactory = await createRumInjector(WASM_PATH);
  });

  it('loads the middleware factory', () => {
    assert.ok(typeof rumFactory === 'function', 'createRumInjector should return a factory function');
  });

  it('injects into text/html responses', async () => {
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

    const middleware = rumFactory(VALID_CONFIG);

    const req = {};
    const res = createMockRes();
    const html = '<html><head><title>Chunked</title></head><body>Streaming!</body></html>';
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

  it('throws on invalid JSON config', async () => {

    assert.throws(
      () => rumFactory({ notValid: true }),
      /Failed to create snippet/,
      'should throw for config missing required fields',
    );
  });

  it('throws on unsupported major version', async () => {

    assert.throws(
      () => rumFactory({
        majorVersion: 1,
        rum: {
          applicationId: 'foo',
          clientToken: 'bar',
          site: 'datadoghq.com',
        },
      }),
      /Failed to create snippet/,
      'should throw for unsupported major version',
    );
  });

  it('preserves body and does not inject when HTML has no </head>', async () => {

    const middleware = rumFactory(VALID_CONFIG);

    const req = {};
    const res = createMockRes();
    const html = '<html><body>No head tag here</body></html>';

    await new Promise((resolve) => {
      middleware(req, res, () => {
        res.setHeader('Content-Type', 'text/html');
        res.end(html);
        resolve();
      });
    });

    const body = res.getBody();
    assert.ok(body.includes('No head tag here'), 'original content should be preserved');
    assert.ok(!body.includes('datadog-rum'), 'should not inject when no </head> found');
    assert.ok(body.length > html.length, 'output should be padded with spaces to preserve content-length');
  });

  it('injects when </head> is split across streamed chunks', async () => {

    const middleware = rumFactory(VALID_CONFIG);

    const req = {};
    const res = createMockRes();
    const html = '<html><head><title>Test</title></head><body>Hello</body></html>';
    // Split in the middle of </head> -> "...title></h" | "ead>..."
    const splitPoint = html.indexOf('</head>') + 3;
    const chunk1 = html.substring(0, splitPoint);
    const chunk2 = html.substring(splitPoint);

    await new Promise((resolve) => {
      middleware(req, res, () => {
        res.setHeader('Content-Type', 'text/html');
        res.write(chunk1);
        res.end(chunk2);
        resolve();
      });
    });

    const body = res.getBody();
    assert.ok(body.includes('datadog-rum'), 'should inject even when </head> is split across chunks');
    assert.ok(body.includes('<body>Hello</body>'), 'body content should be preserved');
  });

  it('skips injection when content-type header is absent', async () => {

    const middleware = rumFactory(VALID_CONFIG);

    const req = {};
    const res = createMockRes();
    const html = '<html><head></head><body>No CT</body></html>';

    await new Promise((resolve) => {
      middleware(req, res, () => {
        // No content-type header set
        res.end(html);
        resolve();
      });
    });

    const body = res.getBody();
    assert.equal(body, html, 'body should pass through unchanged without content-type');
    assert.ok(!body.includes('datadog-rum'), 'should not inject without content-type header');
  });

  it('removes content-length header when injecting', async () => {

    const middleware = rumFactory(VALID_CONFIG);

    const req = {};
    const res = createMockRes();
    const html = '<html><head></head><body></body></html>';

    await new Promise((resolve) => {
      middleware(req, res, () => {
        res.setHeader('Content-Type', 'text/html');
        res.setHeader('Content-Length', String(html.length));
        res.end(html);
        resolve();
      });
    });

    const body = res.getBody();
    assert.ok(body.includes('datadog-rum'), 'injection should occur');
    assert.equal(res.getHeader('content-length'), undefined,
      'content-length header should be removed when injecting');
  });

  it('rejects paths without .wasm extension', async () => {
    await assert.rejects(
      () => createRumInjector('/tmp/not-a-wasm-file.txt'),
      /Invalid WASM path/,
    );
  });

  it('rejects non-existent WASM file', async () => {
    await assert.rejects(
      () => createRumInjector('/tmp/nonexistent.wasm'),
      /ENOENT/,
    );
  });

  it('handles multiple sequential requests with same middleware', async () => {
    const middleware = rumFactory(VALID_CONFIG);

    for (let i = 0; i < 3; i++) {
      const res = createMockRes();
      await new Promise((resolve) => {
        middleware({}, res, () => {
          res.setHeader('Content-Type', 'text/html');
          res.end(`<html><head></head><body>Request ${i}</body></html>`);
          resolve();
        });
      });
      const body = res.getBody();
      assert.ok(body.includes('datadog-rum'), `request ${i} should have injection`);
      assert.ok(body.includes(`Request ${i}`), `request ${i} body should be preserved`);
    }
  });

  it('handles res.end() with no arguments after write', async () => {
    const middleware = rumFactory(VALID_CONFIG);
    const res = createMockRes();

    await new Promise((resolve) => {
      middleware({}, res, () => {
        res.setHeader('Content-Type', 'text/html');
        res.write('<html><head></head><body>Empty end</body></html>');
        res.end();
        resolve();
      });
    });

    assert.ok(res._ended, 'response should be ended');
    assert.ok(res.getBody().includes('datadog-rum'), 'should inject via write path');
  });

  it('supports res.end(callback) calling convention', async () => {
    const middleware = rumFactory(VALID_CONFIG);
    const res = createMockRes();
    let callbackCalled = false;

    await new Promise((resolve) => {
      middleware({}, res, () => {
        res.setHeader('Content-Type', 'text/html');
        res.write('<html><head></head><body>CB test</body></html>');
        res.end(() => {
          callbackCalled = true;
          resolve();
        });
      });
    });

    assert.ok(callbackCalled, 'end callback should be invoked');
    assert.ok(res._ended, 'response should be ended');
    assert.ok(res.getBody().includes('datadog-rum'), 'injection should occur');
  });

  it('invokes res.end(callback) for non-HTML responses', async () => {
    const middleware = rumFactory(VALID_CONFIG);
    const res = createMockRes();
    let callbackCalled = false;

    await new Promise((resolve) => {
      middleware({}, res, () => {
        res.setHeader('Content-Type', 'application/json');
        res.end(() => {
          callbackCalled = true;
          resolve();
        });
      });
    });

    assert.ok(callbackCalled, 'end callback should be invoked for non-HTML');
    assert.ok(res._ended, 'response should be ended');
  });
});
