import { createHash } from "node:crypto";
import { readFile, readdir, writeFile } from "node:fs/promises";
import { join, relative, sep } from "node:path";
import { promisify } from "node:util";
import { brotliCompress, constants } from "node:zlib";

const web = process.argv[2];
const split = process.argv[3] === "--split";
const quality = split ? 11 : Number(process.argv[3] ?? 11);
const compress = promisify(brotliCompress);
const version = createHash("sha256");

async function files(directory) {
    const paths = [];
    for (const entry of await readdir(directory, { withFileTypes: true })) {
        const path = join(directory, entry.name);
        if (entry.isDirectory()) {
            paths.push(...await files(path));
        } else if (entry.name.endsWith(".js") || entry.name.endsWith(".wasm")
            || entry.name === "deferred_slots.bin") {
            paths.push(path);
        }
    }
    return paths;
}

for (const path of (await files(web)).sort()) {
    const content = await readFile(path);
    const name = relative(web, path).split(sep).join("/");
    version.update(name + "\0");
    version.update(createHash("sha256").update(content).digest());
    const isWasm = path.endsWith(".wasm");
    const isPrimary = name === "zed_web_workspace_bg.wasm";
    const params = { [constants.BROTLI_PARAM_QUALITY]: split && isWasm && !isPrimary ? 5 : quality };
    if (split && isPrimary) {
        // This mode produced smaller primary Wasm files in the browser builds we measured.
        params[constants.BROTLI_PARAM_MODE] = constants.BROTLI_MODE_FONT;
    }
    if (isWasm) {
        params[constants.BROTLI_PARAM_LGWIN] = 24;
        params[constants.BROTLI_PARAM_SIZE_HINT] = content.length;
    }
    const compressed = await compress(content, { params });
    await writeFile(path + ".br", compressed);
    if (isWasm) {
        console.log(`WASM ${content.length} bytes, Brotli ${compressed.length} bytes`);
    }
}
await writeFile(join(web, "bundle.version"), version.digest("hex") + "\n");
