import type L from 'leaflet';

// GPU-side isochrone renderer. Lat/lon is projected to a static Web-Mercator
// buffer once per city; the vertex shader applies the viewport transform and
// derives colour from travel-time + reachable-fraction. Over-budget /
// unreachable nodes collapse to a clipped zero-size point. CPU per-frame cost
// is one buffer upload + a few uniforms + one drawArrays; pan/zoom on the
// same frame skips the upload via the identity guards on `GLState`.

const DOT_ALPHA = 0.6;

// a_travelTime carries raw seconds; u16::MAX (65535) marks an unreachable node
// in a frame and is culled by the `> u_maxTime` test (it exceeds any budget).
const VERTEX_SRC = `
  precision highp float;

  attribute vec2 a_proj;        // static Web-Mercator position, normalised [0,1]
  attribute float a_travelTime; // seconds; 65535 = unreachable (frame view)
  attribute float a_fraction;   // reachable fraction [0,1]; 1.0 for frame view

  uniform float u_scale;        // 256 * 2^zoom
  uniform vec2 u_origin;        // viewport NW corner, in [0,1] Mercator space
  uniform vec2 u_invHalf;       // (2/cssWidth, 2/cssHeight)
  uniform float u_pointSize;
  uniform float u_maxTime;      // travel-time budget, seconds

  varying vec4 v_color;

  // Warm ramp (green→yellow→orange→dark red): port of travelTimeColor.
  vec3 warmColor(float t) {
    if (t < 0.25) {
      float s = t / 0.25;
      return vec3(s, 1.0, 0.0);
    } else if (t < 0.5) {
      float s = (t - 0.25) / 0.25;
      return vec3(1.0, 1.0 - s * 0.47, 0.0);
    } else if (t < 0.75) {
      float s = (t - 0.5) / 0.25;
      return vec3(1.0 - s * (119.0 / 255.0), (136.0 / 255.0) * (1.0 - s), 0.0);
    }
    float s = (t - 0.75) / 0.25;
    return vec3((136.0 - s * 68.0) / 255.0, 0.0, 0.0);
  }

  // Cool ramp (cyan→blue→purple→dark purple): port of coolColor.
  vec3 coolColor(float t) {
    if (t < 0.25) {
      float s = t / 0.25;
      return vec3(s * 60.0, 200.0 - s * 80.0, 255.0) / 255.0;
    } else if (t < 0.5) {
      float s = (t - 0.25) / 0.25;
      return vec3(60.0 + s * 40.0, 120.0 - s * 80.0, 255.0 - s * 35.0) / 255.0;
    } else if (t < 0.75) {
      float s = (t - 0.5) / 0.25;
      return vec3(100.0 - s * 20.0, 40.0 - s * 40.0, 220.0 - s * 60.0) / 255.0;
    }
    float s = (t - 0.75) / 0.25;
    return vec3(80.0 - s * 40.0, 0.0, 160.0 - s * 80.0) / 255.0;
  }

  void main() {
    // Cull order matters: an unreachable node in the average view has
    // fraction 0 AND a NaN travel time. Test fraction first and return, so the
    // NaN never reaches the (NaN-unsafe) maxTime comparison below.
    if (a_fraction <= 0.0) {
      gl_Position = vec4(2.0, 2.0, 2.0, 1.0);
      gl_PointSize = 0.0;
      return;
    }
    if (a_travelTime > u_maxTime) {
      gl_Position = vec4(2.0, 2.0, 2.0, 1.0);
      gl_PointSize = 0.0;
      return;
    }

    // Subtract the viewport origin BEFORE scaling: a_proj and u_origin are both
    // in [0,1] and close together for on-screen nodes, so the difference stays
    // f32-clean. Doing scale*a_proj - scale*origin instead would subtract two
    // ~3e7 values and lose precision. Residual error is sub-pixel except at the
    // very highest zoom, where it is ~1-2px on a semi-transparent dot.
    vec2 px = (a_proj - u_origin) * u_scale;
    gl_Position = vec4(px.x * u_invHalf.x - 1.0, 1.0 - px.y * u_invHalf.y, 0.0, 1.0);
    gl_PointSize = u_pointSize;

    float t = clamp(a_travelTime / u_maxTime, 0.0, 1.0);
    float f = clamp(a_fraction, 0.0, 1.0);
    // 2D colour: warm when fully reachable, cool when rarely reachable.
    vec3 rgb = mix(coolColor(t), warmColor(t), f);
    v_color = vec4(rgb, ${DOT_ALPHA});
  }`;

const FRAGMENT_SRC = `
  precision mediump float;
  varying vec4 v_color;
  void main() {
    gl_FragColor = v_color;
  }`;

export interface GLState {
  canvas: HTMLCanvasElement;
  gl: WebGLRenderingContext;
  program: WebGLProgram;
  /** Static Web-Mercator node positions; uploaded once per city. */
  projBuffer: WebGLBuffer;
  /** Per-frame travel times (u16 for a frame, f32 for the average view). */
  travelBuffer: WebGLBuffer;
  /** Per-query reachable fractions; only used by the average view. */
  fractionBuffer: WebGLBuffer;
  loc: {
    aProj: number;
    aTravel: number;
    aFraction: number;
    uScale: WebGLUniformLocation | null;
    uOrigin: WebGLUniformLocation | null;
    uInvHalf: WebGLUniformLocation | null;
    uPointSize: WebGLUniformLocation | null;
    uMaxTime: WebGLUniformLocation | null;
  };
  // Identity guards — skip a buffer re-upload when the source array is the
  // same object as last time (a pan/zoom re-renders the same data).
  uploadedNodes: Float32Array | null;
  nodeCount: number;
  uploadedTravel: Uint16Array | Float32Array | null;
  uploadedFractionSource: Float32Array | null;
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

  function compile(type: number, src: string): WebGLShader {
    const s = gl!.createShader(type);
    if (!s) throw new Error('Failed to create shader');
    gl!.shaderSource(s, src);
    gl!.compileShader(s);
    if (!gl!.getShaderParameter(s, gl!.COMPILE_STATUS)) {
      throw new Error('Shader compile failed: ' + gl!.getShaderInfoLog(s));
    }
    return s;
  }
  const program = gl.createProgram();
  if (!program) throw new Error('Failed to create program');
  gl.attachShader(program, compile(gl.VERTEX_SHADER, VERTEX_SRC));
  gl.attachShader(program, compile(gl.FRAGMENT_SHADER, FRAGMENT_SRC));
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    throw new Error('Program link failed: ' + gl.getProgramInfoLog(program));
  }
  gl.useProgram(program);

  const projBuffer = gl.createBuffer();
  const travelBuffer = gl.createBuffer();
  const fractionBuffer = gl.createBuffer();
  if (!projBuffer || !travelBuffer || !fractionBuffer) {
    throw new Error('Failed to create buffers');
  }

  return {
    canvas,
    gl,
    program,
    projBuffer,
    travelBuffer,
    fractionBuffer,
    loc: {
      aProj: gl.getAttribLocation(program, 'a_proj'),
      aTravel: gl.getAttribLocation(program, 'a_travelTime'),
      aFraction: gl.getAttribLocation(program, 'a_fraction'),
      uScale: gl.getUniformLocation(program, 'u_scale'),
      uOrigin: gl.getUniformLocation(program, 'u_origin'),
      uInvHalf: gl.getUniformLocation(program, 'u_invHalf'),
      uPointSize: gl.getUniformLocation(program, 'u_pointSize'),
      uMaxTime: gl.getUniformLocation(program, 'u_maxTime'),
    },
    uploadedNodes: null,
    nodeCount: 0,
    uploadedTravel: null,
    uploadedFractionSource: null,
  };
}

/** Viewport expressed in normalised Mercator space (origin = NW / scale). */
interface Viewport {
  renderBounds: L.LatLngBounds;
  scale: number;
  originX: number;
  originY: number;
  invW2: number;
  invH2: number;
  dotSize: number;
  dpr: number;
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
    renderBounds,
    scale,
    // topLeft is in pixel space; dividing by scale puts the origin in the same
    // normalised [0,1] Mercator space as the precomputed `a_proj` attribute.
    originX: topLeft.x / scale,
    originY: topLeft.y / scale,
    invW2: 2 / w,
    invH2: 2 / h,
    dotSize,
    dpr,
  };
}

// Project node lat/lon to normalised Web-Mercator and upload as a static
// buffer. The transcendental projection runs once per city, never per frame.
function ensureProjBuffer(glState: GLState, nodeCoords: Float32Array): void {
  if (glState.uploadedNodes === nodeCoords) return;
  const { gl } = glState;
  const numNodes = nodeCoords.length / 2;
  const proj = new Float32Array(numNodes * 2);
  for (let i = 0; i < numNodes; i++) {
    const lat = nodeCoords[i * 2];
    const lon = nodeCoords[i * 2 + 1];
    proj[i * 2] = lon / 360 + 0.5;
    proj[i * 2 + 1] = 0.5 - Math.log(Math.tan(Math.PI / 4 + (lat * Math.PI) / 360)) / (2 * Math.PI);
  }
  gl.bindBuffer(gl.ARRAY_BUFFER, glState.projBuffer);
  gl.bufferData(gl.ARRAY_BUFFER, proj, gl.STATIC_DRAW);
  glState.uploadedNodes = nodeCoords;
  glState.nodeCount = numNodes;
}

function drawScene(glState: GLState, v: Viewport, maxTimeSec: number): RenderResult {
  const { gl, loc } = glState;

  gl.bindBuffer(gl.ARRAY_BUFFER, glState.projBuffer);
  gl.enableVertexAttribArray(loc.aProj);
  gl.vertexAttribPointer(loc.aProj, 2, gl.FLOAT, false, 0, 0);

  gl.uniform1f(loc.uScale, v.scale);
  gl.uniform2f(loc.uOrigin, v.originX, v.originY);
  gl.uniform2f(loc.uInvHalf, v.invW2, v.invH2);
  // gl_PointSize is in physical pixels; scale by dpr so a dot occupies a
  // constant number of CSS pixels regardless of display density.
  gl.uniform1f(loc.uPointSize, v.dotSize * v.dpr);
  gl.uniform1f(loc.uMaxTime, maxTimeSec);

  gl.drawArrays(gl.POINTS, 0, glState.nodeCount);
  gl.flush();

  return { canvas: glState.canvas, renderBounds: v.renderBounds };
}

// Render a single departure-time frame: `frame[i]` is the node's travel time
// in seconds, `65535` if unreachable for this departure.
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
  const { gl, loc } = glState;

  ensureProjBuffer(glState, nodeCoords);

  // Per-frame travel times: the raw u16 array goes straight onto the GPU; the
  // shader reads each value as a float (UNSIGNED_SHORT, non-normalised).
  gl.bindBuffer(gl.ARRAY_BUFFER, glState.travelBuffer);
  if (glState.uploadedTravel !== frame) {
    gl.bufferData(gl.ARRAY_BUFFER, frame, gl.DYNAMIC_DRAW);
    glState.uploadedTravel = frame;
  }
  gl.enableVertexAttribArray(loc.aTravel);
  gl.vertexAttribPointer(loc.aTravel, 1, gl.UNSIGNED_SHORT, false, 0, 0);

  // A frame has no "fraction" dimension — every reachable node counts fully.
  // Feed a constant generic attribute instead of a buffer.
  gl.disableVertexAttribArray(loc.aFraction);
  gl.vertexAttrib1f(loc.aFraction, 1.0);

  return drawScene(glState, v, maxTimeSec);
}

// Render the window-average view: `travelTimes[i]` is the node's mean travel
// time (NaN if never reachable), `sampleCounts[i]` the quantised reachable
// fraction over `totalSamples`.
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
  const { gl, loc } = glState;

  ensureProjBuffer(glState, nodeCoords);

  gl.bindBuffer(gl.ARRAY_BUFFER, glState.travelBuffer);
  if (glState.uploadedTravel !== travelTimes) {
    gl.bufferData(gl.ARRAY_BUFFER, travelTimes, gl.DYNAMIC_DRAW);
    glState.uploadedTravel = travelTimes;
  }
  gl.enableVertexAttribArray(loc.aTravel);
  gl.vertexAttribPointer(loc.aTravel, 1, gl.FLOAT, false, 0, 0);

  // Reachable fraction per node. Built once per query (keyed on the travelTimes
  // identity, which co-changes with sampleCounts) — a pan reuses it.
  if (glState.uploadedFractionSource !== travelTimes) {
    const numNodes = glState.nodeCount;
    const fraction = new Float32Array(numNodes);
    const useFraction = sampleCounts != null && totalSamples != null && totalSamples > 1;
    for (let i = 0; i < numNodes; i++) {
      fraction[i] = useFraction ? sampleCounts![i] / totalSamples! : 1.0;
    }
    gl.bindBuffer(gl.ARRAY_BUFFER, glState.fractionBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, fraction, gl.DYNAMIC_DRAW);
    glState.uploadedFractionSource = travelTimes;
  } else {
    gl.bindBuffer(gl.ARRAY_BUFFER, glState.fractionBuffer);
  }
  gl.enableVertexAttribArray(loc.aFraction);
  gl.vertexAttribPointer(loc.aFraction, 1, gl.FLOAT, false, 0, 0);

  return drawScene(glState, v, maxTimeSec);
}
