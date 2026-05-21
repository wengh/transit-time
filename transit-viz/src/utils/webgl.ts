import { isochroneColor, travelTimeColor } from './colors';
import type L from 'leaflet';

// u16::MAX marks an unreachable node in a `travel_times_at` frame.
const UNREACHABLE = 65535;

// Per-node dot opacity. The average view is semi-transparent so overlapping
// dots from the fraction dimension read as a gradient; a single animation
// frame is a hard reachable/not-reachable cut, so it is drawn more opaque.
const AVG_ALPHA = 153;
const FRAME_ALPHA = 200;

export interface GLState {
  canvas: HTMLCanvasElement;
  gl: WebGLRenderingContext;
  program: WebGLProgram;
  posBuffer: WebGLBuffer;
  colorBuffer: WebGLBuffer;
}

export interface RenderResult {
  canvas: HTMLCanvasElement;
  renderBounds: L.LatLngBounds;
}

export function initWebGL(): GLState | null {
  const canvas = document.createElement('canvas');
  const gl = canvas.getContext('webgl', {
    alpha: true,
    premultipliedAlpha: false,
    antialias: false,
  });
  if (!gl) return null;

  const vsrc = `
    attribute vec2 a_pos;
    attribute vec4 a_color;
    uniform float u_pointSize;
    varying vec4 v_color;
    void main() {
      gl_Position = vec4(a_pos, 0.0, 1.0);
      gl_PointSize = u_pointSize;
      v_color = a_color;
    }`;
  const fsrc = `
    precision mediump float;
    varying vec4 v_color;
    void main() {
      gl_FragColor = v_color;
    }`;

  function compile(type: number, src: string): WebGLShader {
    const s = gl!.createShader(type);
    if (!s) throw new Error('Failed to create shader');
    gl!.shaderSource(s, src);
    gl!.compileShader(s);
    return s;
  }
  const program = gl.createProgram();
  if (!program) throw new Error('Failed to create program');
  gl.attachShader(program, compile(gl.VERTEX_SHADER, vsrc));
  gl.attachShader(program, compile(gl.FRAGMENT_SHADER, fsrc));
  gl.linkProgram(program);
  gl.useProgram(program);

  const posBuffer = gl.createBuffer();
  const colorBuffer = gl.createBuffer();
  if (!posBuffer || !colorBuffer) throw new Error('Failed to create buffers');

  return {
    canvas,
    gl,
    program,
    posBuffer,
    colorBuffer,
  };
}

// Scratch vertex buffers, reused across every render so the per-frame
// animation loop does not allocate (and trigger GC) at 60 Hz. Grown to fit
// the largest city seen; never shrunk.
let scratchPos = new Float32Array(0);
let scratchCol = new Uint8Array(0);

function ensureScratch(numNodes: number): void {
  if (scratchPos.length < numNodes * 2) {
    scratchPos = new Float32Array(numNodes * 2);
    scratchCol = new Uint8Array(numNodes * 4);
  }
}

// Viewport + GL setup shared by the average and per-frame renderers. Holds the
// Web-Mercator projection constants the per-node loop needs.
interface Viewport {
  glState: GLState;
  renderBounds: L.LatLngBounds;
  w: number;
  h: number;
  dpr: number;
  dotSize: number;
  scale: number;
  ox: number;
  oy: number;
  invW2: number;
  invH2: number;
}

function computeViewport(
  glState: GLState,
  map: L.Map,
  L: typeof import('leaflet')
): Viewport | null {
  const bounds = map.getBounds();
  const zoom = map.getZoom();

  const padLat = (bounds.getNorth() - bounds.getSouth()) * 0.5;
  const padLng = (bounds.getEast() - bounds.getWest()) * 0.5;
  const renderBounds = L.latLngBounds(
    [bounds.getSouth() - padLat, bounds.getWest() - padLng],
    [bounds.getNorth() + padLat, bounds.getEast() + padLng]
  );

  const topLeft = map.project(renderBounds.getNorthWest(), zoom);
  const bottomRight = map.project(renderBounds.getSouthEast(), zoom);
  const w = Math.ceil(bottomRight.x - topLeft.x);
  const h = Math.ceil(bottomRight.y - topLeft.y);
  if (w <= 0 || h <= 0) return null;

  const { canvas, gl } = glState;

  // High-DPI: render at physical pixel density so the isochrone's edge stays
  // sharp on retina screens. Cap at 2× — the perceptual gain past 2x doesn't
  // justify the 9× fragment cost on DPR=3 mobile GPUs.
  const dpr = Math.min(2, window.devicePixelRatio || 1);
  canvas.width = Math.ceil(w * dpr);
  canvas.height = Math.ceil(h * dpr);
  gl.viewport(0, 0, canvas.width, canvas.height);
  gl.clearColor(0, 0, 0, 0);
  gl.clear(gl.COLOR_BUFFER_BIT);
  gl.enable(gl.BLEND);
  gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);

  const scale = 256 * Math.pow(2, zoom);
  const metersPerPx = 40075016 / scale;
  const minPx = 5 / metersPerPx;
  const dotSize = Math.max(minPx, Math.max(2, Math.min(6, 14 - zoom)));

  return {
    glState,
    renderBounds,
    w,
    h,
    dpr,
    dotSize,
    scale,
    ox: topLeft.x,
    oy: topLeft.y,
    invW2: 2 / w,
    invH2: 2 / h,
  };
}

// Upload the first `count` packed vertices from the scratch buffers and draw.
function finishDraw(v: Viewport, count: number): RenderResult | null {
  if (count === 0) return null;
  const { canvas, gl, program, posBuffer, colorBuffer } = v.glState;

  const posLoc = gl.getAttribLocation(program, 'a_pos');
  gl.bindBuffer(gl.ARRAY_BUFFER, posBuffer);
  gl.bufferData(gl.ARRAY_BUFFER, scratchPos.subarray(0, count * 2), gl.DYNAMIC_DRAW);
  gl.enableVertexAttribArray(posLoc);
  gl.vertexAttribPointer(posLoc, 2, gl.FLOAT, false, 0, 0);

  const colorLoc = gl.getAttribLocation(program, 'a_color');
  gl.bindBuffer(gl.ARRAY_BUFFER, colorBuffer);
  gl.bufferData(gl.ARRAY_BUFFER, scratchCol.subarray(0, count * 4), gl.DYNAMIC_DRAW);
  gl.enableVertexAttribArray(colorLoc);
  gl.vertexAttribPointer(colorLoc, 4, gl.UNSIGNED_BYTE, true, 0, 0);

  // gl_PointSize is in viewport (= physical) pixels. Scale by dpr so a dot
  // visually occupies the same number of CSS pixels as it would at DPR=1.
  gl.uniform1f(gl.getUniformLocation(program, 'u_pointSize'), v.dotSize * v.dpr);
  gl.drawArrays(gl.POINTS, 0, count);
  gl.flush();

  return { canvas, renderBounds: v.renderBounds };
}

export function renderIsochrone(
  glState: GLState,
  map: L.Map,
  travelTimes: Float32Array,
  nodeCoords: Float32Array,
  maxTimeSec: number,
  L: typeof import('leaflet'),
  sampleCounts?: Uint32Array | null,
  totalSamples?: number
): RenderResult | null {
  if (!travelTimes || !map || !nodeCoords) return null;
  const v = computeViewport(glState, map, L);
  if (!v) return null;

  const numNodes = nodeCoords.length / 2;
  ensureScratch(numNodes);
  const { w, h, dotSize, scale, ox, oy, invW2, invH2 } = v;
  let count = 0;

  for (let i = 0; i < numNodes; i++) {
    const tt = travelTimes[i];
    if (!(tt >= 0 && tt <= maxTimeSec)) continue;

    const fraction =
      sampleCounts != null && totalSamples != null && totalSamples > 1
        ? sampleCounts[i] / totalSamples
        : 1.0;
    const color = isochroneColor(tt, maxTimeSec, fraction);
    const ci2 = i * 2;
    const lat = nodeCoords[ci2];
    const lon = nodeCoords[ci2 + 1];

    const x = scale * (lon / 360 + 0.5) - ox;
    const y =
      scale * (0.5 - Math.log(Math.tan(Math.PI / 4 + (lat * Math.PI) / 360)) / (2 * Math.PI)) - oy;

    if (x < -dotSize || x > w + dotSize || y < -dotSize || y > h + dotSize) continue;

    const ci = count * 2;
    scratchPos[ci] = x * invW2 - 1;
    scratchPos[ci + 1] = 1 - y * invH2;

    const cc = count * 4;
    scratchCol[cc] = color[0];
    scratchCol[cc + 1] = color[1];
    scratchCol[cc + 2] = color[2];
    scratchCol[cc + 3] = AVG_ALPHA;

    count++;
  }

  return finishDraw(v, count);
}

// Render a single departure-time frame: a 1D travel-time ramp with unreachable
// nodes culled outright, so the reachable area visibly pulses as the playhead
// advances. `frame[i]` is the node's travel time in seconds, `UNREACHABLE` if
// the node cannot be reached for this departure.
export function renderIsochroneFrame(
  glState: GLState,
  map: L.Map,
  frame: Uint16Array,
  nodeCoords: Float32Array,
  maxTimeSec: number,
  L: typeof import('leaflet')
): RenderResult | null {
  if (!frame || !map || !nodeCoords) return null;
  const v = computeViewport(glState, map, L);
  if (!v) return null;

  const numNodes = nodeCoords.length / 2;
  ensureScratch(numNodes);
  const { w, h, dotSize, scale, ox, oy, invW2, invH2 } = v;
  let count = 0;

  for (let i = 0; i < numNodes; i++) {
    const tt = frame[i];
    if (tt === UNREACHABLE || tt > maxTimeSec) continue;

    const color = travelTimeColor(tt, maxTimeSec);
    const ci2 = i * 2;
    const lat = nodeCoords[ci2];
    const lon = nodeCoords[ci2 + 1];

    const x = scale * (lon / 360 + 0.5) - ox;
    const y =
      scale * (0.5 - Math.log(Math.tan(Math.PI / 4 + (lat * Math.PI) / 360)) / (2 * Math.PI)) - oy;

    if (x < -dotSize || x > w + dotSize || y < -dotSize || y > h + dotSize) continue;

    const ci = count * 2;
    scratchPos[ci] = x * invW2 - 1;
    scratchPos[ci + 1] = 1 - y * invH2;

    const cc = count * 4;
    scratchCol[cc] = color[0];
    scratchCol[cc + 1] = color[1];
    scratchCol[cc + 2] = color[2];
    scratchCol[cc + 3] = FRAME_ALPHA;

    count++;
  }

  return finishDraw(v, count);
}
