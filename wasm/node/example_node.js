const fs = require('node:fs');
const path = require('node:path');

function allocString(exports, str) {
  const encoder = new TextEncoder();
  const encoded = encoder.encode(str + "\0");

  if (typeof exports.malloc !== 'function') {
    throw new Error("WASM module does not export 'malloc'. Cannot allocate memory for arguments.");
  }

  const ptr = exports.malloc(encoded.length);
  if (ptr === 0) {
    throw new Error(`WASM malloc failed: could not allocate ${encoded.length} bytes`);
  }
  const memory = new Uint8Array(exports.memory.buffer);
  memory.set(encoded, ptr);
  return { ptr, len: encoded.length };
}

function readCString(exports, ptr) {
  const u8 = new Uint8Array(exports.memory.buffer);
  const maxEnd = Math.min(ptr + 65536, u8.length);
  let end = ptr;
  while (end < maxEnd && u8[end] !== 0) end++;
  return new TextDecoder().decode(u8.slice(ptr, end));
}

async function main() {
  const wasmPath = path.resolve(
    __dirname,
    "../../target/wasm32-unknown-unknown/debug/inject_browser_sdk_ffi.wasm",
  );
  console.log(`Loading WASM from ${wasmPath}...`);

  try {
    const wasmCode = fs.readFileSync(wasmPath);
    const wasmModule = await WebAssembly.compile(wasmCode);
    const wasmInstance = await WebAssembly.instantiate(wasmModule, {
      env: {}
    });

    const exports = wasmInstance.exports;
    console.log("WASM Exports:", Object.keys(exports));

    const requiredExports = ['snippet_create_from_json', 'snippet_cleanup', 'malloc', 'free',
                             'memory', 'injector_create', 'injector_write', 'injector_end',
                             'injector_cleanup'];
    for (const name of requiredExports) {
        if (!exports[name]) {
            console.error(`Error: required export '${name}' not found.`);
            process.exit(1);
        }
    }

    // Example config
    const config = {
        majorVersion: 5,
        rum: {
            applicationId: "my-app-id",
            clientToken: "my-client-token",
            site: "datadoghq.com"
        }
    };

    try {
        const { ptr: configPtr } = allocString(exports, JSON.stringify(config));

        console.log("Creating snippet...");
        const snippet = exports.snippet_create_from_json(configPtr);

        // Snippet layout (repr(C), wasm32, total 12 bytes):
        //   offset 0: error_code    (u8, 1 byte + 3 bytes padding)
        //   offset 4: error_message (ptr, 4 bytes)
        //   offset 8: length        (u32, 4 bytes)
        const memory = new DataView(exports.memory.buffer);
        const errorCode = memory.getUint8(snippet);
        console.log(`Snippet created. Error code: ${errorCode}`);

        if (errorCode === 0) {
            const length = memory.getUint32(snippet + 8, true);
            console.log(`Snippet length: ${length}`);

            // Use injector to write snippet
            console.log("\nTesting Injector...");

            const injector = exports.injector_create(snippet);

            const htmlChunk = "<head><title>Test</title></head><body>Hello</body>";
            const { ptr: chunkPtr } = allocString(exports, htmlChunk);
            // injector_write takes an explicit byte length via slice::from_raw_parts,
            // so pass the string byte length without the null terminator.
            const inputLen = new TextEncoder().encode(htmlChunk).length;

            // In wasm32 C ABI, InjectorResult (12 bytes) is returned via a caller-allocated
            // pointer passed as the first argument (sret convention):
            //   injector_write(result_ptr, injector_ptr, chunk_ptr, length)
            const resultPtr = exports.malloc(12);

            exports.injector_write(resultPtr, injector, chunkPtr, inputLen);

            // InjectorResult layout (repr(C), wasm32):
            //   slices_length: u32 @ 0
            //   slices: u32        @ 4
            //   injected: bool     @ 8
            const resMem = new DataView(exports.memory.buffer);
            const slicesLen = resMem.getUint32(resultPtr, true);
            const slicesPtr = resMem.getUint32(resultPtr + 4, true);
            const injected = resMem.getUint8(resultPtr + 8) !== 0;

            console.log(`Injector Result: injected=${injected}, slices=${slicesLen}`);

            // BytesSlice layout (repr(C), wasm32, stride 12):
            //   length: u32 @ 0, start: u32 @ 4, from_incoming_chunk: bool @ 8
            let output = "";
            for(let i=0; i<slicesLen; i++) {
                const sliceOffset = slicesPtr + (i * 12);
                const slLen = resMem.getUint32(sliceOffset, true);
                const slStart = resMem.getUint32(sliceOffset + 4, true);

                if (slLen > 0) {
                    const sliceData = new Uint8Array(exports.memory.buffer).slice(slStart, slStart + slLen);
                    output += new TextDecoder().decode(sliceData);
                }
            }
            console.log("Reconstructed Output:\n", output);

            // Cleanup
            exports.free(resultPtr);
            exports.free(chunkPtr);
            exports.injector_cleanup(injector);

        } else {
            const errorMsgPtr = memory.getUint32(snippet + 4, true);
            const errorMsg = readCString(exports, errorMsgPtr);
            console.error("Failed to create snippet:", errorMsg);
        }

        exports.snippet_cleanup(snippet);
        exports.free(configPtr);

    } catch (e) {
        console.error("Error during WASM snippet/injector operations:", e);
    }

  } catch (e) {
    console.error("Failed to load or run WASM:", e);
    process.exit(1);
  }
}

main();
