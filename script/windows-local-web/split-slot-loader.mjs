export function attachDeferredSlots(data, table, invoke) {
    let offset = 0;
    function byte() {
        if (offset >= data.length) throw new Error("Truncated Wasm slot map");
        return data[offset++];
    }

    function readNumber() {
        let value = 0;
        for (let shift = 0; shift < 35; shift += 7) {
            const part = byte();
            value += (part & 127) * 2 ** shift;
            if ((part & 128) === 0) return value;
        }
        throw new Error("Invalid Wasm slot map integer");
    }

    function valueTypes(count) {
        const start = offset;
        for (let index = 0; index < count; index += 1) {
            const kind = byte();
            if (kind === 0x63 || kind === 0x64) readNumber();
        }
        return data.subarray(start, offset);
    }

    function number(target, value) {
        do {
            const part = value & 127;
            value >>>= 7;
            target.push(part | (value ? 128 : 0));
        } while (value);
    }

    function constant(target, value) {
        target.push(0x41);
        for (;;) {
            const part = value & 127;
            value >>= 7;
            if ((value === 0 && !(part & 64)) || (value === -1 && (part & 64))) {
                target.push(part);
                return;
            }
            target.push(part | 128);
        }
    }

    function name(target, value) {
        number(target, value.length);
        for (let index = 0; index < value.length; index += 1) target.push(value.charCodeAt(index));
    }

    const signatures = [];
    const typeCount = readNumber();
    const types = [];
    number(types, typeCount * 2);
    for (let index = 0; index < typeCount; index += 1) {
        const params = readNumber();
        const input = valueTypes(params);
        const results = readNumber();
        const output = valueTypes(results);
        signatures.push({ params, input, results, output });
    }
    for (const needsSlot of [true, false]) {
        for (const type of signatures) {
            types.push(0x60);
            number(types, type.params + Number(needsSlot));
            if (needsSlot) types.push(0x7f);
            types.push(...type.input);
            number(types, type.results);
            types.push(...type.output);
        }
    }

    const imports = [];
    const dispatch = { table };
    number(imports, typeCount + 1);
    name(imports, "slots");
    name(imports, "table");
    imports.push(1, 0x70, 0, 0);
    for (let index = 0; index < typeCount; index += 1) {
        const field = "f" + index;
        dispatch[field] = (encoded, ...args) => invoke("part" + (encoded >>> 17), encoded & 131071, args);
        name(imports, "slots");
        name(imports, field);
        imports.push(0);
        number(imports, index);
    }

    const functions = [];
    const code = [];
    const positions = new Int32Array(table.length).fill(-1);
    const partCount = readNumber();
    let total = 0;
    for (let index = 0; index < partCount; index += 1) {
        const part = readNumber();
        const entries = readNumber();
        let slot = 0;
        for (let entry = 0; entry < entries; entry += 1) {
            slot += readNumber();
            const type = readNumber();
            if (slot >= positions.length || positions[slot] !== -1 || type >= signatures.length) {
                throw new Error("Invalid deferred slot " + slot);
            }
            positions[slot] = typeCount + total;
            total += 1;
            number(functions, typeCount + type);
            const body = [0];
            constant(body, part * 131072 + slot);
            for (let param = 0; param < signatures[type].params; param += 1) {
                body.push(0x20);
                number(body, param);
            }
            body.push(0x10);
            number(body, type);
            body.push(0x0b);
            number(code, body.length);
            code.push(...body);
        }
    }
    if (offset !== data.length) throw new Error("Unexpected data after Wasm slot map");

    const elements = [];
    let segmentCount = 0;
    for (let first = 0; first < positions.length; first += 1) {
        if (positions[first] < 0) continue;
        let stop = first + 1;
        while (stop < positions.length && positions[stop] >= 0) stop += 1;
        segmentCount += 1;
        elements.push(0);
        constant(elements, first);
        elements.push(0x0b);
        number(elements, stop - first);
        for (let slot = first; slot < stop; slot += 1) number(elements, positions[slot]);
        first = stop - 1;
    }

    function section(tag, body, count) {
        const start = [tag];
        const prefix = [];
        if (count !== undefined) number(prefix, count);
        number(start, prefix.length + body.length);
        const bytes = new Uint8Array(start.length + prefix.length + body.length);
        bytes.set(start);
        bytes.set(prefix, start.length);
        bytes.set(body, start.length + prefix.length);
        return bytes;
    }

    const sections = [Uint8Array.of(0, 97, 115, 109, 1, 0, 0, 0), section(1, types), section(2, imports),
        section(3, functions, total), section(9, elements, segmentCount), section(10, code, total)];
    const adapter = new Uint8Array(sections.reduce((size, part) => size + part.length, 0));
    let start = 0;
    for (const part of sections) {
        adapter.set(part, start);
        start += part.length;
    }
    // Active element segments install typed functions into the primary module's table.
    new WebAssembly.Instance(new WebAssembly.Module(adapter), { slots: dispatch });
}
