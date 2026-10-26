/**
 * JSON 纯数据边界（Phase 0）。
 *
 * `cloneJsonValue` 只接受能经 `JSON.stringify` / `JSON.parse` 无损往返的值：
 * - 拒绝 `undefined`、`function`、`symbol`、`bigint` 与非有限数（NaN/Infinity）；
 * - 拒绝 Date/Map/Set/Promise/class 实例等非普通对象；
 * - 拒绝循环引用、稀疏数组、数组额外字段、符号键、不可枚举字段与访问器；
 * - 接受的值会深拷贝并逐层冻结，调用方无法再通过原始输入影响结果。
 *
 * 数值 `-0` 归一化为 `0`，避免 JSON 往返后无法深度相等。
 * 非法值抛 `NonJsonValueError`，由调用方转换为领域错误（`invalid_action`）。
 */

type JsonValue =
    | null | boolean | number | string
    | readonly JsonValue[]
    | { readonly [key: string]: JsonValue };

export class NonJsonValueError extends Error {
    constructor(readonly path: string, readonly reason: string) {
        super(`${path}: ${reason}`);
        this.name = "NonJsonValueError";
    }
}

/** 校验并深拷贝为冻结的 JSON 数据；`path` 用于指出首个非法值的位置。 */
export function cloneJsonValue<T = JsonValue>(value: unknown, path = "value"): T {
    return clone(value, path, new WeakSet<object>()) as T;
}

function clone(value: unknown, path: string, ancestors: WeakSet<object>): JsonValue {
    switch (typeof value) {
        case "boolean":
        case "string":
            return value;
        case "number":
            if (!Number.isFinite(value)) return fail(path, `${String(value)} is not a finite number`);
            return value === 0 ? 0 : value;
        case "undefined":
            return fail(path, "undefined is not a JSON value");
        case "bigint":
            return fail(path, "bigint is not a JSON value");
        case "symbol":
            return fail(path, "symbol is not a JSON value");
        case "function":
            return fail(path, "function is not a JSON value");
        case "object": {
            if (value === null) return null;
            if (ancestors.has(value)) return fail(path, "circular reference is not JSON data");
            ancestors.add(value);
            try {
                return Array.isArray(value)
                    ? cloneArray(value, path, ancestors)
                    : cloneObject(value, path, ancestors);
            } finally {
                ancestors.delete(value);
            }
        }
        default:
            return fail(path, `unsupported value type: ${typeof value}`);
    }
}

function cloneArray(value: readonly unknown[], path: string, ancestors: WeakSet<object>): readonly JsonValue[] {
    if (Object.getOwnPropertySymbols(value).length > 0) {
        fail(path, "symbol keys are not JSON data");
    }
    for (const key of Object.getOwnPropertyNames(value)) {
        if (key === "length") continue;
        if (!isArrayIndex(key) || Number(key) >= value.length) {
            fail(`${path}.${key}`, "extra array properties are not JSON data");
        }
        const descriptor = Object.getOwnPropertyDescriptor(value, key);
        if (descriptor && !("value" in descriptor)) {
            fail(`${path}[${key}]`, "accessor array elements are not JSON data");
        }
    }

    const result: JsonValue[] = [];
    for (let index = 0; index < value.length; index++) {
        const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
        if (descriptor === undefined) {
            fail(`${path}[${index}]`, "sparse array holes are not JSON data");
        }
        result.push(clone(descriptor.value, `${path}[${index}]`, ancestors));
    }
    return Object.freeze(result);
}

function cloneObject(value: object, path: string, ancestors: WeakSet<object>): { readonly [key: string]: JsonValue } {
    if (!isPlainObjectPrototype(Object.getPrototypeOf(value) as object | null)) {
        fail(path, `${objectLabel(value)} is not JSON data; only plain objects are allowed`);
    }
    if (Object.getOwnPropertySymbols(value).length > 0) {
        fail(path, "symbol keys are not JSON data");
    }

    const result: { [key: string]: JsonValue } = {};
    for (const key of Object.getOwnPropertyNames(value)) {
        const descriptor = Object.getOwnPropertyDescriptor(value, key);
        if (descriptor === undefined) continue;
        if (!descriptor.enumerable) {
            fail(`${path}.${key}`, "non-enumerable properties are not JSON data");
        }
        if (!("value" in descriptor)) {
            fail(`${path}.${key}`, "accessor properties are not JSON data");
        }
        // 用 defineProperty 而非赋值，避免 `__proto__` 等键改写克隆对象的原型。
        Object.defineProperty(result, key, {
            value: clone(descriptor.value, `${path}.${key}`, ancestors),
            enumerable: true,
            writable: true,
            configurable: true,
        });
    }
    return Object.freeze(result);
}

function isPlainObjectPrototype(proto: object | null): boolean {
    if (proto === null) return true;
    // 普通对象（含跨 realm）的原型是某 realm 的 Object.prototype，
    // 其自身原型为 null 且 constructor 为 Object；class 实例的原型还会指向 Object.prototype。
    if (Object.getPrototypeOf(proto) !== null) return false;
    const constructor = Object.getOwnPropertyDescriptor(proto, "constructor");
    return constructor !== undefined
        && "value" in constructor
        && typeof constructor.value === "function"
        && constructor.value.name === "Object";
}

/** 只在原型链的 constructor 数据属性上取名字，不触发用户 getter。 */
function objectLabel(value: object): string {
    const proto = Object.getPrototypeOf(value) as object | null;
    const constructor = proto ? Object.getOwnPropertyDescriptor(proto, "constructor") : undefined;
    const name = constructor && "value" in constructor && typeof constructor.value === "function"
        ? constructor.value.name
        : "";
    return name.length > 0 ? `${name} instance` : "non-plain object";
}

function isArrayIndex(key: string): boolean {
    const index = Number(key);
    return Number.isInteger(index)
        && index >= 0
        && index < 0xffff_ffff
        && String(index) === key;
}

function fail(path: string, reason: string): never {
    throw new NonJsonValueError(path, reason);
}
