// Two zoom conventions coexist in this app:
//
//  - "slippy" zoom — the 256px-tile scale used by OSM links, `cities/*.jsonc`
//    and this app's URL hash. The hash keeps it so existing shared links stay
//    valid and so `zoom`/`center` stay comparable to openstreetmap.org URLs.
//  - MapLibre zoom — 512px tiles. The same ground scale sits one level lower.
const SLIPPY_ZOOM_OFFSET = 1;

export function slippyToMapZoom(z: number): number {
  return z - SLIPPY_ZOOM_OFFSET;
}

export function mapToSlippyZoom(z: number): number {
  return z + SLIPPY_ZOOM_OFFSET;
}
