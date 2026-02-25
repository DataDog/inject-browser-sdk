const fs = require('fs');

/**
 * Loads the Wasm module and returns the middleware factory.
 * @param {string} wasmPath - Path to the .wasm file.
 * @returns {Promise<(config: {majorVersion: number, rum: {applicationId: string, clientToken: string, site?: string}}) => (req, res, next) => void>}
 */
async function createRumInjector(wasmPath) {
    const wasmBuffer = fs.readFileSync(wasmPath);
    const { instance } = await WebAssembly.instantiate(wasmBuffer);
    const exports = instance.exports;
    const memory = exports.memory;

    const requiredExports = ['memory', 'malloc', 'free', 'snippet_create_from_json',
                             'snippet_cleanup', 'injector_create', 'injector_write',
                             'injector_end', 'injector_cleanup'];
    for (const name of requiredExports) {
        if (!exports[name]) {
            throw new Error(`WASM module is missing required export: '${name}'. Ensure the correct WASM file is being loaded.`);
        }
    }

    const encoder = new TextEncoder();

    function readCString(ptr) {
        const u8 = new Uint8Array(memory.buffer);
        const maxEnd = Math.min(ptr + 65536, u8.length);
        let end = ptr;
        while (end < maxEnd && u8[end] !== 0) end++;
        return new TextDecoder().decode(u8.slice(ptr, end));
    }

    function writeString(str) {
        const bytes = encoder.encode(str);
        // +1 for null terminator. The Rust FFI function uses CStr::from_ptr which expects a
        // null-terminated string.
        const ptr = exports.malloc(bytes.length + 1);
        if (ptr === 0) {
            throw new Error(`WASM malloc failed: could not allocate ${bytes.length + 1} bytes`);
        }
        const buffer = new Uint8Array(memory.buffer, ptr, bytes.length + 1);
        buffer.set(bytes);
        buffer[bytes.length] = 0;
        return ptr;
    }

    // InjectorResult layout (repr(C), wasm32, total 12 bytes):
    //   offset 0: slices_length (u32, 4 bytes)
    //   offset 4: slices        (ptr, 4 bytes)
    //   offset 8: injected      (bool, 1 byte + 3 bytes padding)
    function readInjectorResult(ptr) {
        const view = new DataView(memory.buffer);
        const slicesLength = view.getUint32(ptr, true);
        const slicesPtr = view.getUint32(ptr + 4, true);
        const injected = view.getUint8(ptr + 8) !== 0;

        // BytesSlice layout (repr(C), wasm32, stride 12):
        //   offset 0: length            (u32)
        //   offset 4: start             (ptr)
        //   offset 8: from_incoming_chunk (bool)
        const slices = [];
        for (let i = 0; i < slicesLength; i++) {
            const sliceOffset = slicesPtr + (i * 12);
            const length = view.getUint32(sliceOffset, true);
            const start = view.getUint32(sliceOffset + 4, true);
            const data = new Uint8Array(memory.buffer, start, length).slice();
            slices.push(data);
        }

        return { slices, injected };
    }

    return function(config) {
        const configStr = JSON.stringify(config);
        const configPtr = writeString(configStr);
        const snippetPtr = exports.snippet_create_from_json(configPtr);

        // Snippet layout (repr(C), wasm32, total 12 bytes):
        //   offset 0: error_code    (u8, 1 byte + 3 bytes padding)
        //   offset 4: error_message (ptr, 4 bytes)
        //   offset 8: length        (u32, 4 bytes)
        const view = new DataView(memory.buffer);
        const errorCode = view.getUint8(snippetPtr);
        if (errorCode !== 0) {
            const msgPtr = view.getUint32(snippetPtr + 4, true);
            const msg = readCString(msgPtr);
            exports.free(configPtr);
            exports.snippet_cleanup(snippetPtr);
            throw new Error(`Failed to create snippet: ${msg}`);
        }

        // Free the config string. Per API docs: "The json pointer is owned by the caller
        // and should be freed by the caller."
        exports.free(configPtr);

        // snippetPtr is reused across all requests. It is created once and cleaned up when
        // the middleware factory is no longer needed. See Snippet documentation.

        return function middleware(req, res, next) {
            const originalWrite = res.write;
            const originalEnd = res.end;

            let injectorPtr = 0;
            let injectorResultPtr = 0;
            let initialized = false;

            function initInjector() {
                if (initialized) return true;

                const contentType = res.getHeader('content-type') || '';
                if (!contentType.includes('text/html')) {
                    return false;
                }

                initialized = true;
                injectorPtr = exports.injector_create(snippetPtr);
                // Allocate space for InjectorResult struct:
                //   slices_length (u32, 4) + slices ptr (u32, 4) + injected (bool, 1) + padding (3) = 12 bytes
                injectorResultPtr = exports.malloc(12);
                if (injectorResultPtr === 0) {
                    exports.injector_cleanup(injectorPtr);
                    injectorPtr = 0;
                    initialized = false;
                    return false;
                }

                // Remove content-length rather than incrementing it by snippet length (as the API docs suggest).
                // Removing is simpler for a streaming middleware where the final output size depends on whether
                // injection actually occurred.
                res.removeHeader('content-length');
                return true;
            }

            function cleanupInjector() {
                if (injectorPtr) {
                    try { exports.injector_cleanup(injectorPtr); } catch (e) { console.error('WASM injector_cleanup failed:', e); }
                    injectorPtr = 0;
                }
                if (injectorResultPtr) {
                    try { exports.free(injectorResultPtr); } catch (e) { console.error('WASM free failed:', e); }
                    injectorResultPtr = 0;
                }
                initialized = false;
            }

            res.write = function(chunk, encoding, callback) {
                if (!initInjector()) {
                    return originalWrite.apply(res, arguments);
                }

                if (typeof chunk === 'string') {
                    chunk = Buffer.from(chunk, encoding);
                }

                try {
                    const chunkPtr = exports.malloc(chunk.length);
                    if (chunkPtr === 0) {
                        throw new Error(`WASM malloc failed: could not allocate ${chunk.length} bytes`);
                    }
                    // chunk is guaranteed to be a Buffer at this point
                    new Uint8Array(memory.buffer, chunkPtr, chunk.length).set(chunk);

                    // In wasm32 C ABI, InjectorResult (12 bytes) is returned via a caller-allocated
                    // pointer passed as the first argument (sret convention).
                    exports.injector_write(injectorResultPtr, injectorPtr, chunkPtr, chunk.length);
                    const result = readInjectorResult(injectorResultPtr);
                    exports.free(chunkPtr);

                    let writeResult = true;
                    for (const slice of result.slices) {
                        writeResult = originalWrite.call(res, slice);
                    }

                    if (callback) callback();
                    return writeResult;
                } catch (err) {
                    cleanupInjector();
                    return originalWrite.apply(res, arguments);
                }
            };

            res.end = function(chunk, encoding, callback) {
                if (typeof chunk === 'function') {
                    callback = chunk;
                    chunk = null;
                    encoding = null;
                } else if (typeof encoding === 'function') {
                    callback = encoding;
                    encoding = null;
                }

                if (!initialized && chunk) {
                    initInjector();
                }

                if (!initialized) {
                    return originalEnd.apply(res, arguments);
                }

                try {
                    if (chunk) {
                        res.write(chunk, encoding);
                    }

                    exports.injector_end(injectorResultPtr, injectorPtr);
                    const result = readInjectorResult(injectorResultPtr);

                    for (const slice of result.slices) {
                        originalWrite.call(res, slice);
                    }

                    cleanupInjector();
                    return originalEnd.call(res, callback);
                } catch (err) {
                    cleanupInjector();
                    return originalEnd.call(res, callback);
                }
            };

            next();
        };
    };
}

module.exports = createRumInjector;
