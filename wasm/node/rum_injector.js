const fs = require('fs');
const path = require('path');

/**
 * Loads the Wasm module and returns the middleware factory.
 * @param {string} wasmPath - Path to the .wasm file.
 * @returns {Promise<(config: object) => (req, res, next) => void>}
 */
async function createRumInjector(wasmPath) {
    const wasmBuffer = fs.readFileSync(wasmPath);
    const { instance } = await WebAssembly.instantiate(wasmBuffer);
    const exports = instance.exports;
    const memory = exports.memory;

    // Helper to allocate memory
    function malloc(size) {
        return exports.malloc(size);
    }

    // Helper to free memory
    function free(ptr) {
        exports.free(ptr);
    }

    // Helper to write string to Wasm memory
    function writeString(str) {
        const encoder = new TextEncoder();
        const bytes = encoder.encode(str);
        // +1 for null terminator if needed, though snippet_create_from_json uses length from CStr?
        // Rust CStr expects null terminator.
        const ptr = malloc(bytes.length + 1);
        const buffer = new Uint8Array(memory.buffer, ptr, bytes.length + 1);
        buffer.set(bytes);
        buffer[bytes.length] = 0; // Null terminate
        return ptr;
    }

    // Helper to read InjectorResult
    // InjectorResult layout:
    // 0: slices_length (u32)
    // 4: slices (ptr)
    // 8: injected (bool/u8)
    function readInjectorResult(ptr) {
        const view = new DataView(memory.buffer);
        const slicesLength = view.getUint32(ptr, true); // Little endian
        const slicesPtr = view.getUint32(ptr + 4, true);
        const injected = view.getUint8(ptr + 8) !== 0;
        
        const slices = [];
        for (let i = 0; i < slicesLength; i++) {
            // BytesSlice layout:
            // 0: length (u32)
            // 4: start (ptr)
            // 8: from_incoming_chunk (bool/u8)
            // Stride: 12
            const sliceOffset = slicesPtr + (i * 12);
            const length = view.getUint32(sliceOffset, true);
            const start = view.getUint32(sliceOffset + 4, true);
            
            // Copy data out of Wasm memory
            // We must copy because the memory buffer might change/grow or be reused
            const data = new Uint8Array(memory.buffer, start, length).slice();
            slices.push(data);
        }

        return { slices, injected };
    }

    return function(config) {
        const configStr = JSON.stringify(config);
        const configPtr = writeString(configStr);
        const snippetPtr = exports.snippet_create_from_json(configPtr);
        
        // Check for error
        // Snippet layout:
        // 0: error_code (u8)
        // 4: error_message (ptr) - assuming padding after u8
        // 8: length (u32)
        // Wait, struct alignment.
        // pub struct Snippet {
        //     pub error_code: u8,
        //     pub error_message: *const c_char,
        //     pub length: u32,
        // }
        // u8 (1) -> padding (3) -> ptr (4) -> u32 (4). Total 12?
        // Or u8 (1) -> ptr (4) (unaligned? No, usually aligned).
        // Let's check Rust repr(C) alignment rules for this.
        // u8, pointer(4), u32(4). Max align 4.
        // error_code @ 0.
        // error_message @ 4 (padding 1..3).
        // length @ 8.
        
        const view = new DataView(memory.buffer);
        const errorCode = view.getUint8(snippetPtr);
        if (errorCode !== 0) {
            const msgPtr = view.getUint32(snippetPtr + 4, true);
            // Read C string
            let msg = "";
            let p = msgPtr;
            const u8 = new Uint8Array(memory.buffer);
            while (u8[p] !== 0) {
                msg += String.fromCharCode(u8[p]);
                p++;
            }
            free(configPtr); // Free the config string
            exports.snippet_cleanup(snippetPtr);
            throw new Error(`Failed to create snippet: ${msg}`);
        }

        free(configPtr); // Free the config string once snippet is created? 
        // "The json pointer is owned by the caller and should be freed by the caller. No reference to the provided memory is kept by the library."
        
        // We keep snippetPtr alive for the lifetime of the middleware (or per request?)
        // Snippet is reusable.

        return function middleware(req, res, next) {
            // Check content type (simple check)
            // We need to capture write/end to inspect headers and body
            
            const originalWrite = res.write;
            const originalEnd = res.end;
            
            let injectorPtr = 0;
            let injectorResultPtr = 0;
            let initialized = false;

            function initInjector() {
                if (initialized) return true;
                
                // Check headers if set
                const contentType = res.getHeader('content-type') || '';
                if (!contentType.includes('text/html')) {
                    return false;
                }

                initialized = true;
                // Initialize injector
                injectorPtr = exports.injector_create(snippetPtr);
                // Allocate space for result struct (12 bytes)
                injectorResultPtr = malloc(12);
                
                // Remove content-length because we are modifying the body
                res.removeHeader('content-length');
                return true;
            }

            res.write = function(chunk, encoding, callback) {
                if (!initInjector()) {
                    return originalWrite.apply(res, arguments);
                }

                if (typeof chunk === 'string') {
                    chunk = Buffer.from(chunk, encoding);
                }

                // Write chunk to Wasm memory
                // We assume chunk is Buffer or Uint8Array
                const chunkPtr = malloc(chunk.length);
                const chunkBuffer = new Uint8Array(memory.buffer, chunkPtr, chunk.length);
                chunkBuffer.set(chunk);

                // Call injector_write
                // injector_write(ret_ptr, injector_ptr, chunk_ptr, length)
                exports.injector_write(injectorResultPtr, injectorPtr, chunkPtr, chunk.length);
                
                // Read result
                const result = readInjectorResult(injectorResultPtr);
                
                // Free the chunk passed to injector
                free(chunkPtr);

                // Write out slices
                let writeResult = true;
                for (const slice of result.slices) {
                    writeResult = originalWrite.call(res, slice);
                }
                
                if (callback) callback();
                return writeResult;
            };

            res.end = function(chunk, encoding, callback) {
                // Normalize arguments
                if (typeof chunk === 'function') {
                    callback = chunk;
                    chunk = null;
                    encoding = null;
                } else if (typeof encoding === 'function') {
                    callback = encoding;
                    encoding = null;
                }

                if (!initialized && chunk) {
                     // Try to init if we have a chunk
                     const contentType = res.getHeader('content-type') || '';
                     if (contentType.includes('text/html')) {
                         initInjector();
                     }
                }

                if (!initialized) {
                    return originalEnd.apply(res, arguments);
                }

                if (chunk) {
                    res.write(chunk, encoding);
                }

                // Call injector_end
                exports.injector_end(injectorResultPtr, injectorPtr);
                
                const result = readInjectorResult(injectorResultPtr);
                
                for (const slice of result.slices) {
                    originalWrite.call(res, slice);
                }

                // Cleanup
                exports.injector_cleanup(injectorPtr);
                free(injectorResultPtr);
                injectorPtr = 0;
                injectorResultPtr = 0;

                if (callback) {
                    return originalEnd.call(res, callback);
                } else {
                    return originalEnd.call(res);
                }
            };

            next();
        };
    };
}

module.exports = createRumInjector;

