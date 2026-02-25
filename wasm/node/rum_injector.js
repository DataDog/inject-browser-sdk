const fs = require('node:fs');
const path = require('node:path');

/**
 * Loads the Wasm module and returns the middleware factory.
 * @param {string} wasmPath - Path to the .wasm file.
 * @returns {Promise<(config: {majorVersion: number, rum: {applicationId: string, clientToken: string, site?: string}}) => (req, res, next) => void>}
 */
async function createRumInjector(wasmPath) {
    const resolvedPath = path.resolve(wasmPath);
    if (!resolvedPath.endsWith('.wasm')) {
        throw new Error(`Invalid WASM path: file must have a .wasm extension`);
    }
    const wasmBuffer = fs.readFileSync(resolvedPath);
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
    const decoder = new TextDecoder();

    function readCString(ptr) {
        if (ptr === 0) return '<null pointer>';
        const u8 = new Uint8Array(memory.buffer);
        if (ptr >= u8.length) {
            throw new Error(`readCString: pointer ${ptr} is out of WASM memory bounds (size: ${u8.length})`);
        }
        const maxLen = 65536;
        const maxEnd = Math.min(ptr + maxLen, u8.length);
        let end = ptr;
        while (end < maxEnd && u8[end] !== 0) end++;
        if (end === maxEnd && (end >= u8.length || u8[end] !== 0)) {
            console.error(`[rum_injector] readCString: no null terminator found within ${maxLen} bytes at ptr=${ptr}, string may be truncated or pointer invalid`);
        }
        return decoder.decode(u8.slice(ptr, end));
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

        if (slicesLength > 4) {
            throw new Error(`readInjectorResult: slicesLength=${slicesLength} exceeds maximum of 4 ` +
                `(ptr=${ptr}, slicesPtr=${slicesPtr}). Possible struct layout mismatch between JS and WASM.`);
        }

        // BytesSlice layout (repr(C), wasm32, stride 12):
        //   offset 0: length            (u32)
        //   offset 4: start             (ptr)
        //   offset 8: from_incoming_chunk (bool)
        const memSize = memory.buffer.byteLength;
        const slices = [];
        for (let i = 0; i < slicesLength; i++) {
            const sliceOffset = slicesPtr + (i * 12);
            const length = view.getUint32(sliceOffset, true);
            const start = view.getUint32(sliceOffset + 4, true);
            if (start + length > memSize) {
                throw new Error(`readInjectorResult: slice[${i}] out of bounds ` +
                    `(start=${start}, length=${length}, memorySize=${memSize})`);
            }
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

        // snippetPtr is reused across all requests. It is created once and intentionally
        // never freed, since the snippet is expected to live for the entire process lifetime.

        return function middleware(req, res, next) {
            const originalWrite = res.write;
            const originalEnd = res.end;

            let injectorPtr = 0;
            let injectorResultPtr = 0;
            let initialized = false;

            function initInjector() {
                if (initialized) return true;

                const contentType = (res.getHeader('content-type') || '').toLowerCase();
                if (!contentType.includes('text/html')) {
                    return false;
                }

                initialized = true;
                injectorPtr = exports.injector_create(snippetPtr);
                // Allocate space for InjectorResult struct:
                //   slices_length (u32, 4) + slices ptr (u32, 4) + injected (bool, 1) + padding (3) = 12 bytes
                injectorResultPtr = exports.malloc(12);
                if (injectorResultPtr === 0) {
                    console.error('[rum_injector] WASM malloc failed: could not allocate InjectorResult. Skipping injection.');
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

            // Core WASM write logic. Throws on error (caller decides fallback strategy).
            function wasmWrite(chunk) {
                let chunkPtr = 0;
                try {
                    chunkPtr = exports.malloc(chunk.length);
                    if (chunkPtr === 0) {
                        throw new Error(`WASM malloc failed: could not allocate ${chunk.length} bytes`);
                    }
                    new Uint8Array(memory.buffer, chunkPtr, chunk.length).set(chunk);

                    // In wasm32 C ABI, InjectorResult (12 bytes) is returned via a caller-allocated
                    // pointer passed as the first argument (sret convention).
                    exports.injector_write(injectorResultPtr, injectorPtr, chunkPtr, chunk.length);
                    const result = readInjectorResult(injectorResultPtr);
                    exports.free(chunkPtr);
                    chunkPtr = 0;

                    let writeResult = true;
                    for (const slice of result.slices) {
                        writeResult = originalWrite.call(res, slice);
                    }
                    return writeResult;
                } catch (err) {
                    if (chunkPtr) {
                        try { exports.free(chunkPtr); } catch (freeErr) { console.error('[rum_injector] Failed to free chunk memory:', freeErr); }
                    }
                    throw err;
                }
            }

            res.write = function(chunk, encoding, callback) {
                if (!initInjector()) {
                    return originalWrite.apply(res, arguments);
                }

                if (typeof chunk === 'string') {
                    chunk = Buffer.from(chunk, encoding);
                }

                try {
                    const writeResult = wasmWrite(chunk);
                    if (callback) callback();
                    return writeResult;
                } catch (err) {
                    console.error('[rum_injector] Error during WASM injection in res.write, falling back to original response:', err);
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
                    return originalEnd.call(res, chunk, encoding, callback);
                }

                try {
                    if (chunk) {
                        if (typeof chunk === 'string') {
                            chunk = Buffer.from(chunk, encoding);
                        }
                        wasmWrite(chunk);
                    }

                    // sret convention, see wasmWrite above.
                    exports.injector_end(injectorResultPtr, injectorPtr);
                    const result = readInjectorResult(injectorResultPtr);

                    for (const slice of result.slices) {
                        originalWrite.call(res, slice);
                    }
                } catch (err) {
                    console.error('[rum_injector] Error during WASM injection in res.end. ' +
                        'The response may be incomplete if injection was partially applied:', err);
                } finally {
                    cleanupInjector();
                }
                return originalEnd.call(res, callback);
            };

            next();
        };
    };
}

module.exports = createRumInjector;
