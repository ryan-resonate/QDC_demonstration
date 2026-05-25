/* tslint:disable */
/* eslint-disable */

/**
 * Wraps `Processor` for JS. Use lifecycle: `new_processor(...)`, repeatedly
 * `processor_feed_*`, then `processor_finish` to retrieve the result.
 */
export class WasmProcessor {
    free(): void;
    [Symbol.dispose](): void;
    feed_csv(bytes: Uint8Array, time_col: number, x_col: number, y_col: number, z_col: number): void;
    feed_dat(bytes: Uint8Array): void;
    /**
     * Feed a TDMS file (entire bytes). The processor extracts X/Y/Z from
     * the named group + channels and runs the kernel.
     */
    feed_tdms(bytes: Uint8Array, group: string, ch_x: string, ch_y: string, ch_z: string): void;
    /**
     * Drain the processor and return the results as a JS object containing
     * Float64Arrays. Subsequent calls panic — call exactly once.
     */
    finish(): any;
    /**
     * Build a processor from a small JS config object:
     *   { lp_freq, pkpk_time, pkpk_fs, filter_kind }  // filter_kind: "butterworth" | "bessel" | "rc_cascade" | "none"
     */
    constructor(config: any);
}

export function csv_inspect_headers(bytes: Uint8Array): any;

export function dat_inspect_headers(bytes: Uint8Array): any;

/**
 * Min/max-per-bucket decimation. Returns a JS object with two Float64Arrays
 * `{ mins, maxs }` of length `n_buckets` (or fewer if `samples.len() < n_buckets`).
 */
export function decimate_minmax(samples: Float64Array, n_buckets: number): any;

/**
 * Load a processed-CSV (already computed pk-pk results from a previous
 * run) and return the columns as Float64Arrays. The UI consumes this
 * directly into the plot stage without going through the rolling-pk-pk
 * pipeline.
 */
export function processed_csv_load(bytes: Uint8Array): any;

export function qdc_core_version(): string;

/**
 * Parse a TDMS file and return all the sample data for the named channel
 * inside the named group, as a `Float64Array`.
 */
export function tdms_channel_samples(bytes: Uint8Array, group: string, channel: string): Float64Array;

/**
 * Parse a TDMS file (entire bytes) and return a JS object describing its
 * structure: `{ properties: {...}, groups: [{ name, properties, channels: [{ name, sample_count, dt_seconds, start_time_us, unit_string }] }] }`.
 *
 * Sample data is NOT included here — call `tdms_channel_samples` to fetch
 * the f64 buffer for a specific channel. This lets the UI cheaply scan
 * headers without paying the channel-allocation cost.
 */
export function tdms_inspect(bytes: Uint8Array): any;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_wasmprocessor_free: (a: number, b: number) => void;
    readonly csv_inspect_headers: (a: number, b: number, c: number) => void;
    readonly dat_inspect_headers: (a: number, b: number, c: number) => void;
    readonly decimate_minmax: (a: number, b: number, c: number) => number;
    readonly processed_csv_load: (a: number, b: number, c: number) => void;
    readonly qdc_core_version: (a: number) => void;
    readonly tdms_channel_samples: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly tdms_inspect: (a: number, b: number, c: number) => void;
    readonly wasmprocessor_feed_csv: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => void;
    readonly wasmprocessor_feed_dat: (a: number, b: number, c: number, d: number) => void;
    readonly wasmprocessor_feed_tdms: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number) => void;
    readonly wasmprocessor_finish: (a: number) => number;
    readonly wasmprocessor_new: (a: number, b: number) => void;
    readonly __wbindgen_export: (a: number, b: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_export3: (a: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_export4: (a: number, b: number, c: number) => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
