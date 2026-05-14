/**
 * Ambient declaration for the Vite `?url` import used inside this
 * package. `tsc` has no built-in knowledge of `?url`-suffixed imports;
 * the declaration below types the specific upstream subpath the adapter
 * loads from, without dragging in the full `vite/client` ambient surface
 * (which would globally augment `import.meta.env` and friends).
 *
 * Runtime resolution is handled by Vite (and by Vitest's Vite layer in
 * unit tests): the `?url` plugin copies the resource into the bundle's
 * asset directory and substitutes a fingerprinted same-origin URL
 * string. The default export is therefore typed as `string`.
 */
declare module "ghostty-web/ghostty-vt.wasm?url" {
  const url: string;
  export default url;
}

/**
 * Ambient declaration for Vite's `?raw` suffix. The
 * `tests/wasmAssetSource.test.ts` source-level pins import the
 * adapter's own `.ts` files with `?raw` so they can inspect the
 * literal source without adding an `@types/node` devDep for
 * `node:fs`.
 */
declare module "*?raw" {
  const text: string;
  export default text;
}
