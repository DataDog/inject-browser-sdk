const http = require('node:http');
const path = require('node:path');
const createRumInjector = require('./rum_injector');

// Path to the compiled WASM file
// Ensure you have built the project with:
// cargo build --target wasm32-unknown-unknown -p inject-browser-sdk-ffi --release --no-default-features --features alloc
const wasmPath = path.resolve(
    __dirname,
    '../../target/wasm32-unknown-unknown/release/inject_browser_sdk_ffi.wasm',
);

const htmlContent = `
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>RUM Injection Test</title>
</head>
<body>
    <h1>RUM Injection Test Page</h1>
    <p>If the injection works, you should see the Datadog RUM script in the head.</p>
    <p>Check the page source or console.</p>
    <hr/>
    <div>
        <h2>Dynamic Content</h2>
        <p>This content is served in chunks.</p>
    </div>
</body>
</html>
`;

async function startServer() {
    console.log('Loading WASM from:', wasmPath);

    let rumFactory;
    try {
        rumFactory = await createRumInjector(wasmPath);
    } catch (error) {
        console.error('Failed to load WASM module:', error);
        console.error('Make sure you have built the WASM module:');
        console.error('cargo build --target wasm32-unknown-unknown -p inject-browser-sdk-ffi --release --no-default-features --features alloc');
        process.exit(1);
    }

    // Create middleware with configuration
    const rumMiddleware = rumFactory({
        majorVersion: 5,
        rum: {
            applicationId: '00000000-0000-0000-0000-000000000000',
            clientToken: 'pub00000000000000000000000000000000',
            site: 'datadoghq.com',
            env: 'local-test',
            service: 'node-injector-demo',
            version: '1.0.0',
            trackUserInteractions: true,
            trackResources: true,
            trackLongTasks: true
        }
    });

    const server = http.createServer((req, res) => {
        const handleRequest = () => {
            if (req.url === '/') {
                res.setHeader('Content-Type', 'text/html; charset=utf-8');

                // Simulate streaming response
                res.write(htmlContent.substring(0, 150));

                setTimeout(() => {
                    res.end(htmlContent.substring(150));
                }, 0);
            } else if (req.url === '/json') {
                res.setHeader('Content-Type', 'application/json');
                res.end(JSON.stringify({ status: 'ok', message: 'This should not be injected' }));
            } else {
                res.writeHead(404);
                res.end('Not Found');
            }
        };

        try {
            rumMiddleware(req, res, handleRequest);
        } catch (err) {
            console.error('Middleware error:', err);
            if (!res.headersSent) {
                res.writeHead(500);
            }
            res.end('Internal Server Error');
        }
    });

    const PORT = 3000;
    server.listen(PORT, () => {
        console.log(`Server running at http://localhost:${PORT}/`);
    });
}

startServer().catch((err) => {
    console.error('Failed to start server:', err);
    process.exit(1);
});
