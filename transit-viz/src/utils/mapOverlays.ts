import type { GeoJSONSource, Map as MapLibreMap } from 'maplibre-gl';
import type { Feature, FeatureCollection, LineString, Point } from 'geojson';
import { IsochroneLayer } from './isochroneLayer';

// Everything the app draws on top of the basemap, as MapLibre sources and
// layers. Owns the data so it can rebuild after a style swap: `setStyle`
// discards every layer (ours included), then fires `style.load`, at which
// point `install()` recreates them from the retained GeoJSON.
//
// Layer order (bottom → top):
//   basemap land/roads → isochrone → city bbox → route lines → basemap labels
//   → transfer dots → destination pin
// Anything above the last geometry layer but below the label layers that
// follow it is under the labels, which is the
// whole point of a vector basemap here: street and place names stay legible
// over the colour wash and the route lines. Point markers go above labels so
// a label never hides a pin. Raster styles have no symbol layers, so there
// everything simply stacks on top of the tiles.

export interface RouteProps {
  kind: 'walk' | 'transit';
  color: string;
}
export type RouteFeature = Feature<LineString, RouteProps>;
export type PointFeature = Feature<Point, { color: string }>;

// Route lines and transfer dots share one source: a hover replaces both at
// once, and each GeoJSON `setData` is a worker round-trip plus a full repaint.
const SRC = {
  bbox: 'tt-bbox',
  routes: 'tt-routes',
  dest: 'tt-dest',
} as const;

const EMPTY: FeatureCollection = { type: 'FeatureCollection', features: [] };

/** App state stores [lat, lng]; GeoJSON and MapLibre want [lng, lat]. */
export function toLngLat([lat, lng]: [number, number]): [number, number] {
  return [lng, lat];
}

export class MapOverlays {
  readonly iso = new IsochroneLayer();

  private bbox: FeatureCollection = EMPTY;
  private routes: FeatureCollection = EMPTY;
  private dest: FeatureCollection = EMPTY;

  constructor(private readonly map: MapLibreMap) {}

  /** (Re)create our sources and layers. Call from every `style.load`. */
  install(): void {
    const map = this.map;
    if (map.getLayer(this.iso.id)) return;

    // Insert above every geometry layer (roads, buildings, boundaries) and
    // below the labels that follow them. "First symbol layer" is not enough:
    // Positron and Voyager place `waterway_label` *below* their road layers,
    // so anchoring there buried the isochrone and routes under the streets.
    const layers = map.getStyle().layers;
    let lastGeometry = -1;
    layers.forEach((l, i) => {
      if (l.type !== 'symbol') lastGeometry = i;
    });
    const firstSymbol = layers[lastGeometry + 1]?.id;

    const addSource = (id: string, data: FeatureCollection) => {
      if (!map.getSource(id)) map.addSource(id, { type: 'geojson', data });
    };
    addSource(SRC.bbox, this.bbox);
    addSource(SRC.routes, this.routes);
    addSource(SRC.dest, this.dest);

    // ── Below labels ──
    map.addLayer(this.iso, firstSymbol);
    map.addLayer(
      {
        id: 'tt-bbox',
        type: 'line',
        source: SRC.bbox,
        paint: { 'line-color': '#666', 'line-width': 1, 'line-dasharray': [4, 6] },
      },
      firstSymbol
    );
    map.addLayer(
      {
        id: 'tt-routes-walk',
        type: 'line',
        source: SRC.routes,
        filter: ['all', ['==', ['geometry-type'], 'LineString'], ['==', ['get', 'kind'], 'walk']],
        layout: { 'line-cap': 'round', 'line-join': 'round' },
        // Dash lengths are in multiples of line width: 6px on / 8px off at 3px.
        paint: { 'line-color': '#888', 'line-width': 3, 'line-dasharray': [2, 2.67] },
      },
      firstSymbol
    );
    map.addLayer(
      {
        id: 'tt-routes-transit',
        type: 'line',
        source: SRC.routes,
        filter: [
          'all',
          ['==', ['geometry-type'], 'LineString'],
          ['==', ['get', 'kind'], 'transit'],
        ],
        layout: { 'line-cap': 'round', 'line-join': 'round' },
        paint: { 'line-color': ['get', 'color'], 'line-width': 4 },
      },
      firstSymbol
    );

    // ── Above labels ──
    map.addLayer({
      id: 'tt-transfers',
      type: 'circle',
      source: SRC.routes,
      filter: ['==', ['geometry-type'], 'Point'],
      paint: {
        'circle-radius': 5,
        'circle-color': ['get', 'color'],
        'circle-opacity': 0.7,
        'circle-stroke-width': 1,
        'circle-stroke-color': ['get', 'color'],
      },
    });
    map.addLayer({
      id: 'tt-dest',
      type: 'circle',
      source: SRC.dest,
      paint: {
        'circle-radius': 6,
        'circle-color': '#4a90d9',
        'circle-stroke-width': 2,
        'circle-stroke-color': '#fff',
      },
    });
  }

  /** Dashed outline of the loaded city's data extent. */
  setBbox(bbox: [number, number, number, number] | null): void {
    if (!bbox) {
      this.bbox = EMPTY;
    } else {
      const [minLon, minLat, maxLon, maxLat] = bbox;
      this.bbox = {
        type: 'FeatureCollection',
        features: [
          {
            type: 'Feature',
            properties: {},
            geometry: {
              type: 'LineString',
              coordinates: [
                [minLon, minLat],
                [maxLon, minLat],
                [maxLon, maxLat],
                [minLon, maxLat],
                [minLon, minLat],
              ],
            },
          },
        ],
      };
    }
    this.setData(SRC.bbox, this.bbox);
  }

  /** Route polylines for the hovered/pinned destination plus transfer dots. */
  setRoutes(lines: RouteFeature[], transfers: PointFeature[]): void {
    this.routes = { type: 'FeatureCollection', features: [...lines, ...transfers] };
    this.setData(SRC.routes, this.routes);
  }

  clearRoutes(): void {
    this.setRoutes([], []);
  }

  /** Pinned-destination marker; `null` hides it. */
  setDest(latLng: [number, number] | null): void {
    this.dest = latLng
      ? {
          type: 'FeatureCollection',
          features: [
            {
              type: 'Feature',
              properties: {},
              geometry: { type: 'Point', coordinates: toLngLat(latLng) },
            },
          ],
        }
      : EMPTY;
    this.setData(SRC.dest, this.dest);
  }

  /** Push to the live source if the style is up; otherwise install() will. */
  private setData(id: string, data: FeatureCollection): void {
    const src = this.map.getSource(id) as GeoJSONSource | undefined;
    src?.setData(data);
  }
}
