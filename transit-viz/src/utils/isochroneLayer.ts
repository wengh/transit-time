import type {
  CustomLayerInterface,
  CustomRenderMethodInput,
  Map as MapLibreMap,
} from 'maplibre-gl';
import { mapToSlippyZoom } from './zoom';

// GPU-side isochrone renderer, implemented as a MapLibre custom layer so it
// draws inside the map's own frame loop and can sit *beneath* the basemap's
// label layers. Lat/lon is projected to Web Mercator once per city; the vertex
// shader applies the map's projection matrix and derives colour from
// travel-time + reachable-fraction. Over-budget / unreachable nodes collapse to
// a clipped zero-size point.
//
// Cost model: MapLibre repaints the *whole* frame whenever anything changes —
// a GeoJSON route update on hover included — so running the point shader over
// every node in the city on each of those frames would make hovering laggy.
// Instead the points are rendered once into an offscreen texture in
// `prerender`, and `render` just composites that texture. The texture is
// redrawn only when the camera (projection matrix), canvas size or the data
// changes, so a hover-triggered repaint costs one full-screen quad.
//
// Precision: node positions are stored relative to a per-city anchor and the
// anchor's translation is folded into the matrix in f64 on the CPU, so the f32
// attribute never holds a large absolute mercator coordinate. A [0,1] mercator
// f32 has ~3e-8 of slop, which is several pixels at street zoom; the relative
// form keeps dots sub-pixel-aligned with the vector roads at every zoom.

const DOT_ALPHA = 0.6;

// a_travelTime carries raw seconds; u16::MAX (65535) marks an unreachable node
// in a frame and is culled by the `> u_maxTime` test (it exceeds any budget).
const POINT_VERTEX_SRC = `
  precision highp float;

  attribute vec2 a_pos;         // Web-Mercator position relative to the city anchor
  attribute float a_travelTime; // seconds; 65535 = unreachable (frame view)
  attribute float a_fraction;   // reachable fraction [0,1]; 1.0 for frame view

  uniform mat4 u_matrix;        // anchor-relative mercator -> clip space
  uniform float u_pointSize;    // physical pixels
  uniform float u_maxTime;      // travel-time budget, seconds

  varying vec4 v_color;

  // Warm ramp (green->yellow->orange->dark red): port of travelTimeColor.
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

  // Cool ramp (cyan->blue->purple->dark purple): port of coolColor.
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

    gl_Position = u_matrix * vec4(a_pos, 0.0, 1.0);
    gl_PointSize = u_pointSize;

    float t = clamp(a_travelTime / u_maxTime, 0.0, 1.0);
    float f = clamp(a_fraction, 0.0, 1.0);
    // 2D colour: warm when fully reachable, cool when rarely reachable.
    vec3 rgb = mix(coolColor(t), warmColor(t), f);
    // Premultiplied alpha: the offscreen texture and MapLibre's compositing
    // both use (ONE, ONE_MINUS_SRC_ALPHA).
    v_color = vec4(rgb * ${DOT_ALPHA}, ${DOT_ALPHA});
  }`;

const POINT_FRAGMENT_SRC = `
  precision mediump float;
  varying vec4 v_color;
  void main() {
    gl_FragColor = v_color;
  }`;

// Full-screen quad that composites the cached texture into the map frame.
const QUAD_VERTEX_SRC = `
  attribute vec2 a_pos;
  varying vec2 v_uv;
  void main() {
    v_uv = a_pos * 0.5 + 0.5;
    gl_Position = vec4(a_pos, 0.0, 1.0);
  }`;

const QUAD_FRAGMENT_SRC = `
  precision mediump float;
  uniform sampler2D u_tex;
  varying vec2 v_uv;
  void main() {
    gl_FragColor = texture2D(u_tex, v_uv);
  }`;

type Mode = 'average' | 'frame';

interface GLResources {
  pointProgram: WebGLProgram;
  /** Anchor-relative Web-Mercator node positions; uploaded once per city. */
  posBuffer: WebGLBuffer;
  /** Per-node travel times (u16 for a frame, f32 for the average view). */
  travelBuffer: WebGLBuffer;
  /** Per-query reachable fractions; only used by the average view. */
  fractionBuffer: WebGLBuffer;
  pointLoc: {
    aPos: number;
    aTravel: number;
    aFraction: number;
    uMatrix: WebGLUniformLocation | null;
    uPointSize: WebGLUniformLocation | null;
    uMaxTime: WebGLUniformLocation | null;
  };
  quadProgram: WebGLProgram;
  quadBuffer: WebGLBuffer;
  quadLoc: { aPos: number; uTex: WebGLUniformLocation | null };
  /** Offscreen cache of the rendered points, at drawing-buffer resolution. */
  fbo: WebGLFramebuffer;
  tex: WebGLTexture;
  texWidth: number;
  texHeight: number;
}

function compileProgram(gl: WebGL2RenderingContext, vert: string, frag: string): WebGLProgram {
  function compile(type: number, src: string): WebGLShader {
    const s = gl.createShader(type);
    if (!s) throw new Error('Failed to create shader');
    gl.shaderSource(s, src);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
      throw new Error('Shader compile failed: ' + gl.getShaderInfoLog(s));
    }
    return s;
  }
  const program = gl.createProgram();
  if (!program) throw new Error('Failed to create program');
  gl.attachShader(program, compile(gl.VERTEX_SHADER, vert));
  gl.attachShader(program, compile(gl.FRAGMENT_SHADER, frag));
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    throw new Error('Program link failed: ' + gl.getProgramInfoLog(program));
  }
  return program;
}

/** Dot diameter in CSS pixels: shrink as the map zooms in, never below ~5 m on the ground. */
function dotSizeCssPx(mapZoom: number): number {
  const z = mapToSlippyZoom(mapZoom);
  const metersPerPx = 40075016 / (256 * Math.pow(2, z));
  const minPx = 5 / metersPerPx;
  return Math.max(minPx, Math.max(2, Math.min(6, 14 - z)));
}

export class IsochroneLayer implements CustomLayerInterface {
  readonly id = 'isochrone';
  readonly type = 'custom' as const;
  readonly renderingMode = '2d' as const;

  private map: MapLibreMap | null = null;
  private res: GLResources | null = null;

  // ── Data. Owned here so it survives style swaps, which tear down and
  //    recreate the GL resources via onRemove/onAdd. ──
  private nodeCoords: Float32Array | null = null;
  private nodeCount = 0;
  private anchor: [number, number] = [0, 0];
  private relPos: Float32Array | null = null;
  private mode: Mode = 'average';
  private travelTimes: Float32Array | null = null;
  private fraction: Float32Array | null = null;
  /** Owned copy: the worker's frame is a view onto WASM memory that the next request overwrites. */
  private frame: Uint16Array | null = null;
  private maxTimeSec = 0;

  // ── Upload guards: which array currently lives in each GPU buffer. ──
  private uploadedPos: Float32Array | null = null;
  private uploadedTravel: Float32Array | Uint16Array | null = null;
  private uploadedFraction: Float32Array | null = null;
  /** `frame` is reused in place, so identity can't tell us when it changed. */
  private frameDirty = false;

  // ── Offscreen cache validity ──
  /** Anything that changes what the points look like flips this. */
  private dataDirty = true;
  /** Projection matrix the cached texture was rendered with. */
  private readonly cachedMatrix = new Float64Array(16);
  private cacheValid = false;

  private readonly matrix = new Float32Array(16);

  // ── CustomLayerInterface ──

  onAdd(map: MapLibreMap, gl: WebGL2RenderingContext): void {
    this.map = map;

    const pointProgram = compileProgram(gl, POINT_VERTEX_SRC, POINT_FRAGMENT_SRC);
    const quadProgram = compileProgram(gl, QUAD_VERTEX_SRC, QUAD_FRAGMENT_SRC);

    const posBuffer = gl.createBuffer();
    const travelBuffer = gl.createBuffer();
    const fractionBuffer = gl.createBuffer();
    const quadBuffer = gl.createBuffer();
    const fbo = gl.createFramebuffer();
    const tex = gl.createTexture();
    if (!posBuffer || !travelBuffer || !fractionBuffer || !quadBuffer || !fbo || !tex) {
      throw new Error('Failed to create GL resources');
    }

    gl.bindBuffer(gl.ARRAY_BUFFER, quadBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);

    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);

    this.res = {
      pointProgram,
      posBuffer,
      travelBuffer,
      fractionBuffer,
      pointLoc: {
        aPos: gl.getAttribLocation(pointProgram, 'a_pos'),
        aTravel: gl.getAttribLocation(pointProgram, 'a_travelTime'),
        aFraction: gl.getAttribLocation(pointProgram, 'a_fraction'),
        uMatrix: gl.getUniformLocation(pointProgram, 'u_matrix'),
        uPointSize: gl.getUniformLocation(pointProgram, 'u_pointSize'),
        uMaxTime: gl.getUniformLocation(pointProgram, 'u_maxTime'),
      },
      quadProgram,
      quadBuffer,
      quadLoc: {
        aPos: gl.getAttribLocation(quadProgram, 'a_pos'),
        uTex: gl.getUniformLocation(quadProgram, 'u_tex'),
      },
      fbo,
      tex,
      texWidth: 0,
      texHeight: 0,
    };
    // Fresh buffers: everything must be re-uploaded and re-rendered.
    this.uploadedPos = null;
    this.uploadedTravel = null;
    this.uploadedFraction = null;
    this.frameDirty = true;
    this.cacheValid = false;
  }

  onRemove(_map: MapLibreMap, gl: WebGL2RenderingContext): void {
    const res = this.res;
    if (res) {
      gl.deleteBuffer(res.posBuffer);
      gl.deleteBuffer(res.travelBuffer);
      gl.deleteBuffer(res.fractionBuffer);
      gl.deleteBuffer(res.quadBuffer);
      gl.deleteFramebuffer(res.fbo);
      gl.deleteTexture(res.tex);
      gl.deleteProgram(res.pointProgram);
      gl.deleteProgram(res.quadProgram);
    }
    this.res = null;
    this.map = null;
    this.cacheValid = false;
  }

  /**
   * Offscreen pass: (re)draw the points into the cache texture when the
   * camera, canvas size or data changed since the last frame. Most frames —
   * e.g. the repaint MapLibre does for every hover-driven route update —
   * leave the cache untouched.
   */
  prerender(gl: WebGL2RenderingContext, options: CustomRenderMethodInput): void {
    const { res, map } = this;
    if (!res || !map || !this.hasDrawableData()) {
      this.cacheValid = false;
      return;
    }
    const w = gl.drawingBufferWidth;
    const h = gl.drawingBufferHeight;
    const m = options.defaultProjectionData.mainMatrix;
    if (
      this.cacheValid &&
      !this.dataDirty &&
      res.texWidth === w &&
      res.texHeight === h &&
      this.sameMatrix(m)
    ) {
      return;
    }

    if (res.texWidth !== w || res.texHeight !== h) {
      gl.bindTexture(gl.TEXTURE_2D, res.tex);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, w, h, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
      gl.bindFramebuffer(gl.FRAMEBUFFER, res.fbo);
      gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, res.tex, 0);
      res.texWidth = w;
      res.texHeight = h;
    }

    // MapLibre marks its GL state cache dirty after this hook, but the
    // framebuffer and viewport it had bound must be restored so the rest of
    // its offscreen pass draws where it expects to.
    const prevFbo = gl.getParameter(gl.FRAMEBUFFER_BINDING) as WebGLFramebuffer | null;
    const prevViewport = gl.getParameter(gl.VIEWPORT) as Int32Array;

    gl.bindFramebuffer(gl.FRAMEBUFFER, res.fbo);
    gl.viewport(0, 0, w, h);
    gl.disable(gl.DEPTH_TEST);
    gl.disable(gl.STENCIL_TEST);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
    gl.clearColor(0, 0, 0, 0);
    gl.clear(gl.COLOR_BUFFER_BIT);

    this.drawPoints(gl, m);

    gl.bindFramebuffer(gl.FRAMEBUFFER, prevFbo);
    gl.viewport(prevViewport[0], prevViewport[1], prevViewport[2], prevViewport[3]);

    for (let i = 0; i < 16; i++) this.cachedMatrix[i] = m[i];
    this.dataDirty = false;
    this.cacheValid = true;
  }

  /** Main pass: composite the cached texture. */
  render(gl: WebGL2RenderingContext, _options: CustomRenderMethodInput): void {
    const res = this.res;
    if (!res || !this.cacheValid) return;

    // MapLibre unbinds its VAO and sets blending (premultiplied alpha), a
    // read-only depth test and no stencil before calling us, and marks its
    // state cache dirty afterwards — so plain attribute setup on the default
    // vertex array is safe here.
    gl.useProgram(res.quadProgram);
    gl.bindBuffer(gl.ARRAY_BUFFER, res.quadBuffer);
    gl.enableVertexAttribArray(res.quadLoc.aPos);
    gl.vertexAttribPointer(res.quadLoc.aPos, 2, gl.FLOAT, false, 0, 0);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, res.tex);
    gl.uniform1i(res.quadLoc.uTex, 0);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  }

  // ── Data setters (called from React / the animation store) ──

  /**
   * Project node lat/lon to anchor-relative Web Mercator. Runs once per city;
   * a repeat call with the same array is a no-op. Any travel-time data from a
   * previous node set is dropped, since it no longer lines up.
   */
  setNodes(nodeCoords: Float32Array): void {
    if (nodeCoords === this.nodeCoords) return;
    this.nodeCoords = nodeCoords;
    const n = nodeCoords.length / 2;
    this.nodeCount = n;

    const proj = new Float64Array(n * 2);
    let sx = 0;
    let sy = 0;
    for (let i = 0; i < n; i++) {
      const lat = nodeCoords[i * 2];
      const lon = nodeCoords[i * 2 + 1];
      const x = lon / 360 + 0.5;
      const y = 0.5 - Math.log(Math.tan(Math.PI / 4 + (lat * Math.PI) / 360)) / (2 * Math.PI);
      proj[i * 2] = x;
      proj[i * 2 + 1] = y;
      sx += x;
      sy += y;
    }
    const ax = n > 0 ? sx / n : 0;
    const ay = n > 0 ? sy / n : 0;
    this.anchor = [ax, ay];
    const rel = new Float32Array(n * 2);
    for (let i = 0; i < n; i++) {
      rel[i * 2] = proj[i * 2] - ax;
      rel[i * 2 + 1] = proj[i * 2 + 1] - ay;
    }
    this.relPos = rel;

    this.travelTimes = null;
    this.fraction = null;
    this.frame = null;
    this.invalidate();
  }

  /**
   * Show the window-average view: `travelTimes[i]` is the node's mean travel
   * time (NaN if never reachable), `sampleCounts[i]` the quantised reachable
   * fraction over `totalSamples`.
   */
  setAverage(
    travelTimes: Float32Array,
    sampleCounts: Uint32Array | null | undefined,
    totalSamples: number | undefined
  ): void {
    this.mode = 'average';
    // Fraction is built once per query, keyed on the travelTimes identity
    // (which co-changes with sampleCounts) — a pan or mode flip reuses it.
    if (travelTimes !== this.travelTimes) {
      this.travelTimes = travelTimes;
      const n = travelTimes.length;
      const fraction = new Float32Array(n);
      const useFraction = sampleCounts != null && totalSamples != null && totalSamples > 1;
      for (let i = 0; i < n; i++) {
        fraction[i] = useFraction ? sampleCounts[i] / totalSamples : 1.0;
      }
      this.fraction = fraction;
    }
    this.invalidate();
  }

  /**
   * Show a single departure-time frame: `frame[i]` is the node's travel time
   * in seconds, `65535` if unreachable for this departure. The array is
   * copied — the caller's buffer is only valid until its next worker call.
   */
  setFrame(frame: Uint16Array): void {
    this.mode = 'frame';
    if (!this.frame || this.frame.length !== frame.length) {
      this.frame = new Uint16Array(frame.length);
    }
    this.frame.set(frame);
    this.frameDirty = true;
    this.invalidate();
  }

  setMaxTime(seconds: number): void {
    if (seconds === this.maxTimeSec) return;
    this.maxTimeSec = seconds;
    this.invalidate();
  }

  /** Drop all travel-time data (city change, source cleared). */
  clear(): void {
    this.travelTimes = null;
    this.fraction = null;
    this.frame = null;
    this.invalidate();
  }

  // ── Internals ──

  private hasDrawableData(): boolean {
    const n = this.nodeCount;
    if (!this.relPos || n === 0) return false;
    if (this.mode === 'frame') return this.frame !== null && this.frame.length === n;
    return this.travelTimes !== null && this.travelTimes.length === n && this.fraction !== null;
  }

  private sameMatrix(m: ArrayLike<number>): boolean {
    const c = this.cachedMatrix;
    for (let i = 0; i < 16; i++) if (c[i] !== m[i]) return false;
    return true;
  }

  /** Bind buffers (uploading what changed) and draw every node as a point. */
  private drawPoints(gl: WebGL2RenderingContext, mainMatrix: ArrayLike<number>): void {
    const res = this.res!;
    const map = this.map!;
    const { pointLoc: loc } = res;
    const relPos = this.relPos!;
    const isFrame = this.mode === 'frame';

    gl.useProgram(res.pointProgram);

    gl.bindBuffer(gl.ARRAY_BUFFER, res.posBuffer);
    if (this.uploadedPos !== relPos) {
      gl.bufferData(gl.ARRAY_BUFFER, relPos, gl.STATIC_DRAW);
      this.uploadedPos = relPos;
    }
    gl.enableVertexAttribArray(loc.aPos);
    gl.vertexAttribPointer(loc.aPos, 2, gl.FLOAT, false, 0, 0);

    gl.bindBuffer(gl.ARRAY_BUFFER, res.travelBuffer);
    if (isFrame) {
      const frame = this.frame!;
      if (this.frameDirty || this.uploadedTravel !== frame) {
        gl.bufferData(gl.ARRAY_BUFFER, frame, gl.DYNAMIC_DRAW);
        this.uploadedTravel = frame;
        this.frameDirty = false;
      }
      // The raw u16 array goes straight onto the GPU; the shader reads each
      // value as a float (UNSIGNED_SHORT, non-normalised).
      gl.enableVertexAttribArray(loc.aTravel);
      gl.vertexAttribPointer(loc.aTravel, 1, gl.UNSIGNED_SHORT, false, 0, 0);
      // A frame has no "fraction" dimension — every reachable node counts
      // fully. Feed a constant generic attribute instead of a buffer.
      gl.disableVertexAttribArray(loc.aFraction);
      gl.vertexAttrib1f(loc.aFraction, 1.0);
    } else {
      const travelTimes = this.travelTimes!;
      if (this.uploadedTravel !== travelTimes) {
        gl.bufferData(gl.ARRAY_BUFFER, travelTimes, gl.DYNAMIC_DRAW);
        this.uploadedTravel = travelTimes;
      }
      gl.enableVertexAttribArray(loc.aTravel);
      gl.vertexAttribPointer(loc.aTravel, 1, gl.FLOAT, false, 0, 0);

      const fraction = this.fraction!;
      gl.bindBuffer(gl.ARRAY_BUFFER, res.fractionBuffer);
      if (this.uploadedFraction !== fraction) {
        gl.bufferData(gl.ARRAY_BUFFER, fraction, gl.DYNAMIC_DRAW);
        this.uploadedFraction = fraction;
      }
      gl.enableVertexAttribArray(loc.aFraction);
      gl.vertexAttribPointer(loc.aFraction, 1, gl.FLOAT, false, 0, 0);
    }

    gl.uniformMatrix4fv(loc.uMatrix, false, this.anchoredMatrix(mainMatrix));
    // gl_PointSize is in physical pixels; scale by the map's pixel ratio so a
    // dot occupies a constant number of CSS pixels regardless of density.
    gl.uniform1f(loc.uPointSize, dotSizeCssPx(map.getZoom()) * map.getPixelRatio());
    gl.uniform1f(loc.uMaxTime, this.maxTimeSec);

    gl.drawArrays(gl.POINTS, 0, this.nodeCount);
  }

  /**
   * `mainMatrix` maps [0,1] Web Mercator to clip space. Fold the anchor
   * translation into its last column (M · T(anchor)) in f64 so the shader
   * only ever multiplies small relative positions by a matrix whose
   * translation is the anchor's on-screen position — both f32-friendly.
   */
  private anchoredMatrix(m: ArrayLike<number>): Float32Array {
    const [ax, ay] = this.anchor;
    const out = this.matrix;
    for (let i = 0; i < 12; i++) out[i] = m[i];
    out[12] = m[0] * ax + m[4] * ay + m[12];
    out[13] = m[1] * ax + m[5] * ay + m[13];
    out[14] = m[2] * ax + m[6] * ay + m[14];
    out[15] = m[3] * ax + m[7] * ay + m[15];
    return out;
  }

  /** Data changed: the cached texture is stale; ask the map for a frame. */
  private invalidate(): void {
    this.dataDirty = true;
    this.map?.triggerRepaint();
  }
}
