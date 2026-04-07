import { travelTimeColor } from './colors';
import type L from 'leaflet';

export interface GLState {
  canvas: HTMLCanvasElement;
  gl: WebGLRenderingContext;
  program: WebGLProgram;
  posBuffer: WebGLBuffer;
  colorBuffer: WebGLBuffer;
}

export interface RenderResult {
  dataUrl: string;
  renderBounds: L.LatLngBounds;
}

export function initWebGL(): GLState | null {
  const canvas = document.createElement('canvas');
  const gl = canvas.getContext('webgl', { alpha: true, premultipliedAlpha: false, antialias: false });
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

export function renderIsochrone(
  glState: GLState,
  map: L.Map,
  travelTimes: Float32Array,
  nodeCoords: Float32Array,
  maxTimeSec: number,
  L: typeof import('leaflet')
): RenderResult | null {
  if (!travelTimes || !map || !nodeCoords) return null;

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

  const { canvas, gl, program, posBuffer, colorBuffer } = glState;

  canvas.width = w;
  canvas.height = h;
  gl.viewport(0, 0, w, h);
  gl.clearColor(0, 0, 0, 0);
  gl.clear(gl.COLOR_BUFFER_BIT);
  gl.enable(gl.BLEND);
  gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);

  const scale = 256 * Math.pow(2, zoom);
  const numNodes = nodeCoords.length / 2;
  const metersPerPx = 40075016 / scale;
  const minPx = 5 / metersPerPx;
  const dotSize = Math.max(minPx, Math.max(2, Math.min(6, 14 - zoom)));
  const ox = topLeft.x;
  const oy = topLeft.y;
  const invW2 = 2 / w;
  const invH2 = 2 / h;

  const positions = new Float32Array(numNodes * 2);
  const colors = new Uint8Array(numNodes * 4);
  let count = 0;

  for (let i = 0; i < numNodes; i++) {
    const tt = travelTimes[i];
    if (!(tt >= 0 && tt <= maxTimeSec)) continue;

    const color = travelTimeColor(tt, maxTimeSec);
    const ci2 = i * 2;
    const lat = nodeCoords[ci2];
    const lon = nodeCoords[ci2 + 1];

    const x = scale * (lon / 360 + 0.5) - ox;
    const y = scale * (0.5 - Math.log(Math.tan(Math.PI / 4 + (lat * Math.PI) / 360)) / (2 * Math.PI)) - oy;

    if (x < -dotSize || x > w + dotSize || y < -dotSize || y > h + dotSize) continue;

    const ci = count * 2;
    positions[ci] = x * invW2 - 1;
    positions[ci + 1] = 1 - y * invH2;

    const cc = count * 4;
    colors[cc] = color[0];
    colors[cc + 1] = color[1];
    colors[cc + 2] = color[2];
    colors[cc + 3] = 153;

    count++;
  }

  if (count === 0) return null;

  const posLoc = gl.getAttribLocation(program, 'a_pos');
  gl.bindBuffer(gl.ARRAY_BUFFER, posBuffer);
  gl.bufferData(gl.ARRAY_BUFFER, positions.subarray(0, count * 2), gl.DYNAMIC_DRAW);
  gl.enableVertexAttribArray(posLoc);
  gl.vertexAttribPointer(posLoc, 2, gl.FLOAT, false, 0, 0);

  const colorLoc = gl.getAttribLocation(program, 'a_color');
  gl.bindBuffer(gl.ARRAY_BUFFER, colorBuffer);
  gl.bufferData(gl.ARRAY_BUFFER, colors.subarray(0, count * 4), gl.DYNAMIC_DRAW);
  gl.enableVertexAttribArray(colorLoc);
  gl.vertexAttribPointer(colorLoc, 4, gl.UNSIGNED_BYTE, true, 0, 0);

  gl.uniform1f(gl.getUniformLocation(program, 'u_pointSize'), dotSize);
  gl.drawArrays(gl.POINTS, 0, count);
  gl.finish();

  return { dataUrl: canvas.toDataURL(), renderBounds };
}
