const fs = require('node:fs');
const path = require('node:path');

// Function to allocate string in WASM memory
function allocString(exports, str) {
  const encoder = new TextEncoder();
  const encoded = encoder.encode(str + "\0"); // Null-terminated
  
  if (typeof exports.malloc !== 'function') {
    throw new Error("WASM module does not export 'malloc'. Cannot allocate memory for arguments.");
  }
  
  const ptr = exports.malloc(encoded.length);
  const memory = new Uint8Array(exports.memory.buffer);
  memory.set(encoded, ptr);
  return { ptr, len: encoded.length };
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
      env: {} // Imports if needed
    });
    
    const exports = wasmInstance.exports;
    console.log("WASM Exports:", Object.keys(exports));

    if (!exports.snippet_create_from_json) {
        console.error("Error: snippet_create_from_json not found in exports.");
        return;
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
        // Note: This call will fail if 'malloc' is not exported by the Rust library.
        const { ptr: configPtr } = allocString(exports, JSON.stringify(config));
        
        console.log("Creating snippet...");
        const snippet = exports.snippet_create_from_json(configPtr);
        
        // Read struct Snippet { error_code: u8, padding, error_message: u32, length: u32 }
        // In WASM32 (32-bit), pointers are 4 bytes aligned to 4 bytes.
        // u8 takes 1 byte. Next field is ptr (4 bytes aligned). So 3 bytes padding.
        // Offset 0: error_code (u8)
        // Offset 4: error_message (*const char)
        // Offset 8: length (u32)
        
        const memory = new DataView(exports.memory.buffer);
        const errorCode = memory.getUint8(snippet); // Offset 0
        console.log(`Snippet created. Error code: ${errorCode}`);
        
        if (errorCode === 0) {
            const length = memory.getUint32(snippet + 8, true); // Offset 8
            const dataPtr = memory.getUint32(snippet + 12, true); // Offset 12
            
            // Check for null pointer or crazy values
            if (dataPtr === 0) {
                console.error("Snippet data pointer is null despite success error code!");
            } else {
                // Now read from dataPtr!
                const snipppetString = new TextDecoder().decode(exports.memory.buffer.slice(dataPtr, dataPtr + length));    
                console.log(`Snippet: ${snipppetString}`);
            }

            // Use injector to write snippet
            console.log("\nTesting Injector...");
            
            // 1. Create Injector
            const injector = exports.injector_create(snippet);
            
            // 2. Simulate HTML chunk (we'll inject into <head>)
            const htmlChunk = "<head><title>Test</title></head><body>Hello</body>";
            const { ptr: chunkPtr, len: chunkLen } = allocString(exports, htmlChunk);
            // allocString adds null terminator, but for injector_write we pass explicit length.
            // Actually allocString adds \0 which increases length by 1. We should pass the original string length if we want to match bytes?
            // But let's pass the full allocated length including \0 or just the string bytes?
            // The Rust slice::from_raw_parts uses the length we pass.
            // If we pass chunkLen (which includes \0), the \0 will be part of the input chunk.
            // Let's pass exactly the string length.
            const inputLen = new TextEncoder().encode(htmlChunk).length;

            // 3. Call injector_write
            // InjectorResult is returned BY VALUE (struct).
            // In WASM C-ABI, large structs returned by value are usually handled by passing a pointer to memory 
            // where the result should be written as the first argument (arg0).
            // Wait, `InjectorResult` has 3 fields (u32, ptr, bool) -> 12 bytes (aligned).
            // It might be returned via pointer.
            
            // Let's check how `injector_write` is exported.
            // Signature: fn injector_write(injector: *mut Injector, chunk: *const u8, length: u32) -> InjectorResult
            
            // In many WASM implementations of C ABI, a struct return implies the first argument is the pointer to the return slot.
            // So signature in JS becomes: injector_write(result_ptr, injector_ptr, chunk_ptr, length)
            
            // We need to allocate space for the result struct.
            // struct InjectorResult { slices_length: u32, slices: *const BytesSlice, injected: bool }
            // Layout (32-bit):
            // u32 (4)
            // ptr (4)
            // bool (1) + padding (3) -> total 12 bytes? 
            // Or maybe aligned to 4.
            
            const resultPtr = exports.malloc(16); // Safe margin
            
            // Call: injector_write(resultPtr, injector, chunkPtr, inputLen)
            exports.injector_write(resultPtr, injector, chunkPtr, inputLen);
            
            // Read result from resultPtr
            const resMem = new DataView(exports.memory.buffer);
            const slicesLen = resMem.getUint32(resultPtr, true);
            const slicesPtr = resMem.getUint32(resultPtr + 4, true);
            const injected = resMem.getUint8(resultPtr + 8) !== 0;
            
            console.log(`Injector Result: injected=${injected}, slices=${slicesLen}`);
            
            // Read slices
            // struct BytesSlice { length: u32, start: *const u8, from_incoming_chunk: bool }
            // Size: 4 + 4 + 1(+3 padding) = 12 bytes.
            
            let output = "";
            for(let i=0; i<slicesLen; i++) {
                const sliceOffset = slicesPtr + (i * 12);
                const slLen = resMem.getUint32(sliceOffset, true);
                const slStart = resMem.getUint32(sliceOffset + 4, true);
                // const fromChunk = resMem.getUint8(sliceOffset + 8);
                
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
            // If error code is not 0, read the error message
            const errorMsgPtr = memory.getUint32(snippet + 4, true); // Offset 4
            
            // Read C-string from errorMsgPtr
            const memBytes = new Uint8Array(exports.memory.buffer);
            let end = errorMsgPtr;
            while (memBytes[end] !== 0) end++;
            const errorMsg = new TextDecoder().decode(memBytes.slice(errorMsgPtr, end));
            
            console.error("Failed to create snippet:", errorMsg);
        }
        
        exports.snippet_cleanup(snippet);
        exports.free(configPtr); // Assuming free exists
        
    } catch (e) {
        console.error("Runtime error (likely missing malloc/free):", e.message);
    }

  } catch (e) {
    console.error("Failed to load or run WASM:", e);
  }
}

main();

