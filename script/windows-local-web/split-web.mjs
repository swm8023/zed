import { spawnSync } from "node:child_process";
import { copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, extname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { attachDeferredSlots } from "./split-slot-loader.mjs";

const [splitter, sourceArgument, destinationArgument, ...startupProfiles] = process.argv.slice(2);
if (!splitter || !sourceArgument || !destinationArgument || startupProfiles.length === 0) {
    throw new Error("Usage: node split-web.mjs <wasm-split> <source artifacts> <new output directory> <startup profile>...");
}

const source = resolve(sourceArgument);
const destination = resolve(destinationArgument);
const sourceWeb = join(source, "web");
const destinationWeb = join(destination, "web");
const moduleName = "zed_web_workspace_bg.wasm";
const glueName = "zed_web_workspace.js";
const primaryPath = join(destinationWeb, moduleName);
const nativeServerPath = join("cargo", "release-fast", "remote-server.exe");
const optimizer = join(dirname(splitter), `wasm-opt${extname(splitter)}`);

function run(binary, args, capture = false) {
    const result = spawnSync(binary, args, {
        encoding: "utf8",
        maxBuffer: 16 * 1024 * 1024,
        stdio: capture ? ["ignore", "pipe", "inherit"] : "inherit",
    });
    if (result.error) throw result.error;
    if (result.status !== 0) throw new Error(`${binary} exited with status ${result.status ?? result.signal}`);
    return result.stdout;
}

function wasmSections(bytes) {
    let cursor = 8;
    return {
        number() {
            let value = 0;
            for (let shift = 0; shift < 35; shift += 7) {
                const byte = bytes.readUInt8(cursor++);
                value += (byte & 127) * 2 ** shift;
                if ((byte & 128) === 0) return value;
            }
            throw new Error("Invalid Wasm integer");
        },
        string() {
            const length = this.number();
            const result = bytes.toString("utf8", cursor, cursor + length);
            cursor += length;
            return result;
        },
        byte() {
            return bytes.readUInt8(cursor++);
        },
        typeVector() {
            const start = cursor;
            const count = this.number();
            for (let index = 0; index < count; index += 1) {
                const kind = this.byte();
                if (kind === 0x63 || kind === 0x64) this.number();
            }
            return bytes.subarray(start, cursor);
        },
        section(id) {
            while (cursor < bytes.length) {
                const tag = this.byte();
                const length = this.number();
                if (tag === id) return;
                cursor += length;
            }
            throw new Error(`Wasm section ${id} is missing`);
        },
        skip(length) {
            cursor += length;
        },
    };
}

function makeManifest(module, profile) {
    const code = wasmSections(module);
    code.section(10);
    const functionCount = code.number();
    const sizes = Array.from({ length: functionCount }, () => {
        const size = code.number();
        code.skip(size);
        return size;
    });
    const rows = [...profile.matchAll(/^([+-]) (\d+)\s*$/gm)];
    const functionIds = new Set(rows.map(([, , id]) => Number(id)));
    if (functionIds.size !== functionCount || rows.length !== functionCount || Array.from(functionIds).some(id => id >= functionCount)) {
        throw new Error("Startup profile does not match the browser Wasm functions");
    }

    const groups = [];
    const deferredIds = rows.filter(([, visited, id]) => visited === "-" && sizes[Number(id)] > 24)
        .map(([, , id]) => Number(id)).sort((left, right) => left - right);
    // A deferred call downloads its entire shard. Keep small helpers in smaller groups.
    const groupsByFunctionSize = [[240000, 2049, Infinity], [72000, 161, 2048], [36000, 0, 160]];
    for (const [limit, minimum, maximum] of groupsByFunctionSize) {
        let functions = [];
        let size = 0;
        for (const id of deferredIds) {
            if (sizes[id] < minimum || sizes[id] > maximum) continue;
            if (functions.length > 0 && size + sizes[id] > limit) {
                groups.push(functions);
                functions = [];
                size = 0;
            }
            functions.push(id);
            size += sizes[id];
        }
        if (functions.length > 0) groups.push(functions);
    }
    if (groups.length === 0) throw new Error("The supplied profile leaves no functions to defer");
    return groups;
}

function encodeNumber(value) {
    const bytes = [];
    do {
        const part = value & 127;
        value >>>= 7;
        bytes.push(part | (value ? 128 : 0));
    } while (value);
    return Buffer.from(bytes);
}

function encodeSlots(module, groups, placeholders) {
    const typeSection = wasmSections(module);
    typeSection.section(1);
    const count = typeSection.number();
    const types = Array.from({ length: count }, () => {
        if (typeSection.byte() !== 0x60) throw new Error("Unsupported non-function Wasm type");
        return Buffer.concat([typeSection.typeVector(), typeSection.typeVector()]);
    });
    const functionSection = wasmSections(module);
    functionSection.section(3);
    const functionCount = functionSection.number();
    const typesByFunction = Array.from({ length: functionCount }, () => functionSection.number());
    const partForFunction = new Map();
    groups.forEach((functions, part) => {
        for (const id of functions) partForFunction.set(id, part);
    });
    const slotsByPart = groups.map(() => []);
    const signatures = [];
    const signatureIds = new Map();
    const knownSlots = new Set();
    for (const [, slotText, functionText] of placeholders.matchAll(/^(\d+):(\d+)\s*$/gm)) {
        const slot = Number(slotText);
        const id = Number(functionText);
        const part = partForFunction.get(id);
        const type = typesByFunction[id];
        if (slot >= 131072 || knownSlots.has(slot) || part === undefined || part >= 16384 || !types[type]) {
            throw new Error(`Unexpected entry for deferred slot ${slot}`);
        }
        knownSlots.add(slot);
        if (!signatureIds.has(type)) {
            signatureIds.set(type, signatures.length);
            signatures.push(types[type]);
        }
        slotsByPart[part].push([slot, signatureIds.get(type)]);
    }
    if (knownSlots.size === 0) throw new Error("The split build contains no deferred slots");
    const parts = [];
    for (const [part, slots] of slotsByPart.entries()) {
        if (slots.length === 0) continue;
        const bytes = [encodeNumber(part), encodeNumber(slots.length)];
        let previous = 0;
        for (const [slot, type] of slots.sort((left, right) => left[0] - right[0])) {
            bytes.push(encodeNumber(slot - previous), encodeNumber(type));
            previous = slot;
        }
        parts.push(Buffer.concat(bytes));
    }
    return Buffer.concat([encodeNumber(signatures.length), ...signatures, encodeNumber(parts.length), ...parts]);
}

function exportedTable(wasm, index) {
    const exports = wasmSections(wasm);
    exports.section(7);
    const count = exports.number();
    for (let number = 0; number < count; number += 1) {
        const name = exports.string();
        const kind = exports.byte();
        const target = exports.number();
        if (kind === 1 && target === index) return name;
    }
    throw new Error("The Wasm split table is not exported");
}

function addLoader(glue, wasm, placeholderMap) {
    const tables = [...placeholderMap.matchAll(/^table (\d+)\s*$/gm)];
    if (tables.length !== 1) throw new Error("Unexpected Wasm split tables");
    const table = exportedTable(wasm, Number(tables[0][1]));
    const anchor = "let wasmModule, wasmInstance, wasm;";
    const assignment = "    wasm = instance.exports;";
    const loading = "    const { instance, module } = await __wbg_load(await module_or_path, imports);";
    for (const marker of [anchor, assignment, loading]) {
        if (glue.split(marker).length !== 2) throw new Error("Unexpected wasm-bindgen initialization code");
    }

    const loader = `\n${attachDeferredSlots.toString()}
let deferredSlotBytes;
const loadedDeferredParts = new Map();
globalThis.zedDeferredParts = [];
function readDeferredBytes(path) {
    const request = new XMLHttpRequest();
    request.open("GET", new URL(path, import.meta.url), false);
    request.overrideMimeType("text/plain; charset=x-user-defined");
    request.send();
    if (request.status !== 200) throw new Error("Could not load deferred Wasm file " + path + ": " + request.status);
    const text = request.responseText;
    const bytes = new Uint8Array(text.length);
    for (let index = 0; index < bytes.length; index += 1) bytes[index] = text.charCodeAt(index);
    return bytes;
}
function callDeferredPart(part, slot, args) {
    if (!loadedDeferredParts.has(part)) {
        const bytes = readDeferredBytes(part + ".wasm");
        const instance = new WebAssembly.Instance(new WebAssembly.Module(bytes), { zed_primary: wasm });
        loadedDeferredParts.set(part, instance);
        globalThis.zedDeferredParts.push({ part, slot });
    }
    return wasm[${JSON.stringify(table)}].get(slot)(...args);
}
`;
    const attach = `\n    attachDeferredSlots(deferredSlotBytes ?? readDeferredBytes("deferred_slots.bin"), wasm[${JSON.stringify(table)}], callDeferredPart);
    deferredSlotBytes = undefined;`;
    const loadInParallel = `    const [{ instance, module }, slotBytes] = await Promise.all([
        Promise.resolve(module_or_path).then(value => __wbg_load(value, imports)),
        fetch(new URL("deferred_slots.bin", import.meta.url)).then(async response => {
            if (!response.ok) throw new Error("Could not load deferred Wasm slots: " + response.status);
            return new Uint8Array(await response.arrayBuffer());
        }),
    ]);
    deferredSlotBytes = slotBytes;`;
    return glue.replace(anchor, anchor + loader).replace(assignment, assignment + attach).replace(loading, loadInParallel);
}

const wasmPath = join(sourceWeb, moduleName);
const input = readFileSync(wasmPath);
const glue = readFileSync(join(sourceWeb, glueName), "utf8");
mkdirSync(destination);
mkdirSync(destinationWeb);
const mergedProfile = join(destination, "startup.data");
const manifest = join(destination, "split-manifest.txt");
run(splitter, ["--merge-profiles", ...startupProfiles, "-o", mergedProfile]);
const profile = run(splitter, [wasmPath, "--print-profile", mergedProfile], true);
const groups = makeManifest(input, profile);
writeFileSync(manifest, groups.map((group, part) => [`part${part}`, ...group].join("\n")).join("\n\n") + "\n");
run(splitter, [wasmPath, "--multi-split", `--manifest=${manifest}`, `--out-prefix=${destinationWeb}/`,
    "--no-placeholders", "--import-namespace", "zed_primary", "--export-prefix", "zed_split_",
    "--placeholdermap", "--strip-debug", "-o", primaryPath]);
const placeholderPath = primaryPath + ".placeholders";
const placeholders = readFileSync(placeholderPath, "utf8");
unlinkSync(placeholderPath);
writeFileSync(join(destinationWeb, "deferred_slots.bin"), encodeSlots(input, groups, placeholders));
writeFileSync(join(destinationWeb, glueName), addLoader(glue, readFileSync(primaryPath), placeholders));
const optimizedPrimary = join(destination, "optimized.wasm");
run(optimizer, [primaryPath, "--duplicate-function-elimination", "--merge-similar-functions", "--vacuum",
    "--strip-debug", "-o", optimizedPrimary]);
renameSync(optimizedPrimary, primaryPath);
copyFileSync(join(sourceWeb, "index.html"), join(destinationWeb, "index.html"));
copyFileSync(join(sourceWeb, "profile.version"), join(destinationWeb, "profile.version"));
const wheelmaker = join(sourceWeb, "wheelmaker.html");
if (existsSync(wheelmaker)) copyFileSync(wheelmaker, join(destinationWeb, "wheelmaker.html"));
for (const directory of ["snippets", "builtin-themes"]) {
    const path = join(sourceWeb, directory);
    if (existsSync(path)) cpSync(path, join(destinationWeb, directory), { recursive: true });
}
run(process.execPath, [fileURLToPath(new URL("pack-web.mjs", import.meta.url)), destinationWeb, "--split"]);
const nativeDestination = join(destination, nativeServerPath);
mkdirSync(dirname(nativeDestination), { recursive: true });
copyFileSync(join(source, nativeServerPath), nativeDestination);
