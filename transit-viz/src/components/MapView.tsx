import React, { forwardRef, useCallback, useEffect, useImperativeHandle, useRef } from 'react';
import L from 'leaflet';
import { useAppState } from '../state/AppContext';
import { initWebGL, renderIsochrone } from '../utils/webgl';
import {
  cancelInflightQuery,
  getProfileHoverData,
  snapToNode,
  type HoverPath,
} from '../utils/router';
import { ROUTE_COLORS } from '../utils/colors';
import { getHashParams, setHashParams } from '../utils/urlHash';
import { getSortedTravelTimes } from '../utils/hoverInfo';
import { resolveMapStyle, DEFAULT_MAP_STYLE } from '../utils/mapStyles';
import { useIsMobile } from '../utils/useIsMobile';

export interface MapViewHandle {
  setSource(lat: number, lng: number): Promise<void>;
  flyTo(lat: number, lng: number): void;
}

const MapView = forwardRef<MapViewHandle>(function MapView(_props, ref): React.ReactNode {
  const { state, dispatch } = useAppState();
  const mapRef = useRef<L.Map | null>(null);
  const mapContainerRef = useRef<HTMLDivElement>(null);
  const glStateRef = useRef<ReturnType<typeof initWebGL> | null>(null);
  const isoOverlayRef = useRef<L.Layer | null>(null);
  const sourceMarkerRef = useRef<L.Marker | null>(null);
  const destMarkerRef = useRef<L.CircleMarker | null>(null);
  const bboxRectRef = useRef<L.Rectangle | null>(null);
  const tileLayerRef = useRef<L.TileLayer | null>(null);
  const routePolylinesRef = useRef<L.Path[]>([]);
  const routeRendererRef = useRef<L.Canvas | null>(null);
  const drawRouteLayersRef = useRef<((paths: HoverPath[]) => void) | null>(null);
  const lastHoveredNodeRef = useRef<number | null>(null);
  const renderIsoRef = useRef<(() => void) | null>(null);

  // Ref to the setSource closure (updated each time the map-events effect runs)
  // so the imperative handle can call it from outside MapView.
  const setSourceRef = useRef<((lat: number, lng: number) => Promise<void>) | null>(null);

  useImperativeHandle(ref, () => ({
    setSource: (lat, lng) => setSourceRef.current?.(lat, lng) ?? Promise.resolve(),
    flyTo: (lat, lng) => {
      const map = mapRef.current;
      if (!map) return;
      map.flyTo([lat, lng], Math.max(map.getZoom(), 14));
    },
  }));

  // Keep current state in refs for event handlers
  const stateRef = useRef(state);
  stateRef.current = state;

  // Width-based mobile detection. Mirrored to a ref so the once-installed
  // Leaflet click handlers can read the live value without being re-registered
  // when the viewport crosses the breakpoint.
  const isMobile = useIsMobile();
  const isMobileRef = useRef(isMobile);
  isMobileRef.current = isMobile;

  // Keep leaflet's double-click-zoom in sync when the user crosses the
  // breakpoint at runtime (e.g. rotating a tablet, resizing a window).
  useEffect(() => {
    const map = mapRef.current;
    if (!map) return;
    if (isMobile) map.doubleClickZoom.enable();
    else map.doubleClickZoom.disable();
  }, [isMobile]);

  // Clear destination marker and routes
  const clearDestination = useCallback(() => {
    if (destMarkerRef.current) {
      destMarkerRef.current.remove();
      destMarkerRef.current = null;
    }
    routePolylinesRef.current.forEach((p) => p.remove());
    routePolylinesRef.current = [];
  }, []);

  // Initialize map
  useEffect(() => {
    if (mapRef.current) return;
    // Desktop uses double-click to set the source, so leaflet's default
    // double-click-to-zoom would conflict. On mobile that gesture is unused,
    // so let leaflet keep its default zoom behavior.
    const map = L.map('map', { doubleClickZoom: isMobileRef.current }).setView([40, -90], 4);
    const initialStyle = resolveMapStyle(DEFAULT_MAP_STYLE);
    tileLayerRef.current = L.tileLayer(initialStyle.url, {
      attribution: initialStyle.attribution,
      maxZoom: 20,
      subdomains: initialStyle.subdomains ?? 'abc',
      crossOrigin: true,
    }).addTo(map);
    // Custom pane above the isochrone ImageOverlay (which lives in overlayPane at z-index 400;
    // this pane at 450 is a sibling stacking context that wins regardless of the image's zIndex).
    map.createPane('transitLines');
    map.getPane('transitLines')!.style.zIndex = '450';

    mapRef.current = map;

    return () => {
      map.remove();
      mapRef.current = null;
    };
  }, []);

  // Swap tile layer when map style changes or system theme changes (for 'default' style)
  useEffect(() => {
    const map = mapRef.current;
    if (!map) return;

    function applyStyle() {
      const style = resolveMapStyle(state.mapStyle);
      if (tileLayerRef.current) tileLayerRef.current.remove();
      tileLayerRef.current = L.tileLayer(style.url, {
        attribution: style.attribution,
        maxZoom: 20,
        subdomains: style.subdomains ?? 'abc',
        crossOrigin: true,
      }).addTo(map!);
    }

    applyStyle();

    if (state.mapStyle === 'default') {
      const mq = window.matchMedia('(prefers-color-scheme: dark)');
      mq.addEventListener('change', applyStyle);
      return () => mq.removeEventListener('change', applyStyle);
    }
  }, [state.mapStyle]);

  // Set up map event handlers
  useEffect(() => {
    const map = mapRef.current as L.Map;
    if (!map) return;

    function getNodeLatLng(node: number): [number, number] | null {
      const coords = stateRef.current.nodeCoords;
      if (!coords) return null;
      return [coords[node * 2], coords[node * 2 + 1]];
    }

    function clearRouteOverlay() {
      routePolylinesRef.current.forEach((p) => p.remove());
      routePolylinesRef.current = [];
    }

    function drawRouteSegments(allPaths: HoverPath[]) {
      clearRouteOverlay();
      if (!routeRendererRef.current) {
        routeRendererRef.current = L.canvas({ pane: 'transitLines', padding: 0.5 });
      }
      const renderer = routeRendererRef.current;
      const routeColorMap: Record<string, string> = {};
      let colorIdx = 0;
      const seenSegments = new Set<string>();
      const seenTransfers = new Set<string>();
      for (const { segments } of allPaths) {
        for (const seg of segments) {
          if (seg.coords.length < 2) continue;
          let color: string, dashArray: string | null, weight: number;
          let coords = seg.coords;
          if (seg.edgeType === 0) {
            // Normalize walk segment direction so the dedup key collapses
            // walks traversing the same edge in either direction.
            const first = coords[0],
              last = coords[coords.length - 1];
            if (first[0] > last[0] || (first[0] === last[0] && first[1] > last[1])) {
              coords = [...coords].reverse();
            }
            color = '#888';
            dashArray = '6, 8';
            weight = 3;
          } else {
            if (!(seg.routeName in routeColorMap)) {
              // Rust's `TransitRouter::route_color` returns the map-legible hex
              // (already luminance-adjusted via `adjust_color_for_visibility`).
              // Empty string means the route has no GTFS colour — fall back to
              // the palette.
              const s = stateRef.current;
              let routeColor = seg.routeIdx < 0xffffffff ? s.routeColors[seg.routeIdx] || '' : '';
              if (!routeColor) {
                routeColor = ROUTE_COLORS[colorIdx % ROUTE_COLORS.length];
              }
              routeColorMap[seg.routeName] = routeColor;
              colorIdx++;
            }
            color = routeColorMap[seg.routeName];
            dashArray = null;
            weight = 4;
          }
          const a = coords[0];
          const b = coords[coords.length - 1];
          const routeKey = seg.edgeType === 0 ? '' : seg.routeIdx;
          const segKey = `${seg.edgeType}|${routeKey}|${a[0]},${a[1]}|${b[0]},${b[1]}|${coords.length}`;
          if (!seenSegments.has(segKey)) {
            seenSegments.add(segKey);
            const line = L.polyline(coords, {
              color,
              weight,
              opacity: 1,
              ...(dashArray ? { dashArray } : {}),
              interactive: false,
              pane: 'transitLines',
              renderer,
            }).addTo(map);
            routePolylinesRef.current.push(line);
          }
          // Add circle at end of transit segments to mark transfers
          if (seg.edgeType === 1) {
            const s = stateRef.current;
            if (s.nodeCoords && seg.endNodeIdx !== undefined) {
              const tKey = `${seg.routeIdx}|${seg.endNodeIdx}`;
              if (!seenTransfers.has(tKey)) {
                seenTransfers.add(tKey);
                const lat = s.nodeCoords[seg.endNodeIdx * 2];
                const lon = s.nodeCoords[seg.endNodeIdx * 2 + 1];
                const circle = L.circleMarker([lat, lon], {
                  radius: 5,
                  color: color,
                  fillColor: color,
                  fillOpacity: 0.7,
                  weight: 1,
                  interactive: false,
                  pane: 'transitLines',
                  renderer,
                }).addTo(map);
                routePolylinesRef.current.push(circle);
              }
            }
          }
        }
      }
    }

    drawRouteLayersRef.current = drawRouteSegments;

    async function showDestination(node: number, pin: boolean) {
      const sAtStart = stateRef.current;
      if (!sAtStart.travelTimes) return;
      const tt = sAtStart.travelTimes[node];
      if (isNaN(tt) || tt < 0) {
        clearRouteOverlay();
        if (destMarkerRef.current) {
          destMarkerRef.current.remove();
          destMarkerRef.current = null;
        }
        dispatch({ type: pin ? 'UNPIN_DESTINATION' : 'CLEAR_HOVER' });
        return;
      }

      const allPaths = await getProfileHoverData(node);

      // Re-verify state after async work. If the source changed or was
      // cleared, this destination data is stale.
      const s = stateRef.current;
      if (!s.travelTimes || s.sourceNode !== sAtStart.sourceNode) return;

      const travelTimes = getSortedTravelTimes(allPaths);

      drawRouteSegments(allPaths.filter((p) => p.segments.length > 0));

      // Analytic per-node summary comes straight from the Rust profile router —
      // avoids re-aggregating from the (Pareto-filtered) `allPaths`, which no
      // longer corresponds to discrete sample counts.
      const avgTravelTime = isFinite(tt) ? tt : null;
      const reachableFraction =
        s.sampleCounts && s.totalSamples > 0 ? s.sampleCounts[node] / s.totalSamples : null;
      const hoverData = { allPaths, travelTimes, avgTravelTime, reachableFraction };

      const latLng = getNodeLatLng(node);
      if (!latLng) return;

      if (pin) {
        if (destMarkerRef.current) {
          destMarkerRef.current.setLatLng(latLng);
        } else {
          destMarkerRef.current = L.circleMarker(latLng, {
            radius: 6,
            color: '#fff',
            fillColor: '#4a90d9',
            fillOpacity: 1,
            weight: 2,
            pane: 'transitLines',
          }).addTo(map);
        }
        dispatch({ type: 'PIN_DESTINATION', node, latLng, hoverData });
      } else {
        dispatch({ type: 'SET_HOVER_DATA', hoverData });
      }
    }

    async function setSource(lat: number, lng: number) {
      const s = stateRef.current;
      if (s.loadingState !== 'ready') return;
      // Cancel any in-flight query *before* awaiting the worker round-trip
      // for snapToNode — otherwise that message queues behind the running
      // compute and the cancel flag isn't flipped until the compute finishes.
      cancelInflightQuery();
      const node = await snapToNode(lat, lng);
      if (node === null) return;
      const latLng = getNodeLatLng(node);
      if (!latLng) return;
      if (sourceMarkerRef.current) {
        sourceMarkerRef.current.setLatLng(latLng);
      } else {
        sourceMarkerRef.current = L.marker(latLng, { title: 'Origin' }).addTo(map);
      }
      // Clear destination
      if (destMarkerRef.current) {
        destMarkerRef.current.remove();
        destMarkerRef.current = null;
      }
      clearRouteOverlay();
      dispatch({ type: 'SET_SOURCE', node, latLng });
    }

    setSourceRef.current = setSource;

    // Desktop: double-click sets source
    let lastPinTime = 0;
    function onDblClick(e: L.LeafletMouseEvent) {
      if (stateRef.current.loadingState !== 'ready') return;
      // Mobile uses the Origin/Dest toggle in the top bar instead.
      if (isMobileRef.current) return;
      setSource(e.latlng.lat, e.latlng.lng);
    }

    // Single click: behavior depends on platform.
    // Desktop: pin/unpin destination. Mobile: routes by interactionMode —
    // 'origin' sets the source, 'dest' pins (or repins) the destination.
    async function onClick(e: L.LeafletMouseEvent) {
      const s = stateRef.current;
      if (s.loadingState !== 'ready') return;

      if (isMobileRef.current) {
        if (s.interactionMode === 'origin') {
          setSource(e.latlng.lat, e.latlng.lng);
          return;
        }
        // Dest mode: replace any existing pin with the tapped node.
        const node = await snapToNode(e.latlng.lat, e.latlng.lng);
        if (node !== null) showDestination(node, true);
        return;
      }

      // Desktop: ignore the second click of a double-click (handled by onDblClick).
      if (e.originalEvent.detail > 1) return;

      if (s.sourceNode === null) return;

      // Desktop: if already pinned, unpin on click (swallow if it's the second
      // click of a double-click so dblclick can set source cleanly).
      if (s.pinnedNode !== null) {
        if (Date.now() - lastPinTime < 300) return;
        dispatch({ type: 'UNPIN_DESTINATION' });
        return;
      }

      const node = await snapToNode(e.latlng.lat, e.latlng.lng);
      if (node !== null) {
        lastPinTime = Date.now();
        showDestination(node, true);
      }
    }

    // Hover: show route (desktop, no pinned dest)
    async function onMouseMove(e: L.LeafletMouseEvent) {
      const s = stateRef.current;
      if (!s.travelTimes || s.pinnedNode !== null) return;

      const node = await snapToNode(e.latlng.lat, e.latlng.lng);
      if (node === lastHoveredNodeRef.current || node === null) return;
      lastHoveredNodeRef.current = node;
      showDestination(node, false);
    }

    function onMouseOut() {
      lastHoveredNodeRef.current = null;
      if (stateRef.current.pinnedNode === null) {
        clearRouteOverlay();
        dispatch({ type: 'CLEAR_HOVER' });
      }
    }

    // Re-render isochrone on map move/zoom
    function renderIso() {
      const s = stateRef.current;
      if (!s.travelTimes || !s.nodeCoords || !map) return;
      if (!glStateRef.current) {
        glStateRef.current = initWebGL();
      }
      if (!glStateRef.current) return;
      const result = renderIsochrone(
        glStateRef.current,
        map,
        s.travelTimes,
        s.nodeCoords,
        s.maxTimeMin * 60,
        L,
        s.sampleCounts,
        s.totalSamples
      );
      if (result) {
        if (isoOverlayRef.current) {
          // Layer already added — just update its bounds and reposition
          (isoOverlayRef.current as any)._isoCanvas = result.canvas;
          (isoOverlayRef.current as any)._isoBounds = result.renderBounds;
          (isoOverlayRef.current as any)._reset();
        } else {
          const CanvasLayer = L.Layer.extend({
            _isoCanvas: result.canvas as HTMLCanvasElement,
            _isoBounds: result.renderBounds as L.LatLngBounds,
            onAdd(m: L.Map) {
              this._map = m;
              this._zoomAnimated = (m as any)._zoomAnimated;
              const pane = m.getPane('overlayPane')!;
              this._isoCanvas.style.position = 'absolute';
              this._isoCanvas.style.pointerEvents = 'none';
              if (this._zoomAnimated) {
                L.DomUtil.addClass(this._isoCanvas, 'leaflet-zoom-animated');
              }
              pane.appendChild(this._isoCanvas);
              this._reset();
              return this;
            },
            onRemove() {
              this._isoCanvas.remove();
              return this;
            },
            getEvents() {
              const events: Record<string, (e: any) => void> = {
                zoom: this._reset,
                viewreset: this._reset,
              };
              if (this._zoomAnimated) {
                events.zoomanim = this._animateZoom;
              }
              return events;
            },
            _animateZoom(e: any) {
              const m: L.Map = this._map;
              const scale = m.getZoomScale(e.zoom);
              const offset = (m as any)._latLngBoundsToNewLayerBounds(
                this._isoBounds,
                e.zoom,
                e.center
              ).min;
              L.DomUtil.setTransform(this._isoCanvas, offset, scale);
            },
            _reset() {
              const m: L.Map = this._map;
              if (!m) return;
              const topLeft = m.latLngToLayerPoint(this._isoBounds.getNorthWest());
              const bottomRight = m.latLngToLayerPoint(this._isoBounds.getSouthEast());
              L.DomUtil.setTransform(this._isoCanvas, topLeft, 1);
              this._isoCanvas.style.width = bottomRight.x - topLeft.x + 'px';
              this._isoCanvas.style.height = bottomRight.y - topLeft.y + 'px';
            },
          });
          isoOverlayRef.current = new (CanvasLayer as any)().addTo(map);
        }
      }
    }

    // Store render function in ref for external trigger
    renderIsoRef.current = renderIso;

    function onMoveEnd() {
      renderIso();
      if (stateRef.current.sourceNode === null) return;
      const c = map.getCenter();
      const current = getHashParams();
      setHashParams({ ...current, zoom: map.getZoom(), center: [c.lat, c.lng] });
    }

    map.on('dblclick', onDblClick);
    map.on('click', onClick);
    map.on('mousemove', onMouseMove);
    map.on('mouseout', onMouseOut);
    map.on('moveend', onMoveEnd);
    map.on('zoomend', onMoveEnd);

    return () => {
      map.off('dblclick', onDblClick);
      map.off('click', onClick);
      map.off('mousemove', onMouseMove);
      map.off('mouseout', onMouseOut);
      map.off('moveend', onMoveEnd);
      map.off('zoomend', onMoveEnd);
    };
  }, [dispatch]);

  // Reposition map on city change
  useEffect(() => {
    const map = mapRef.current;
    const city = state.currentCity;
    if (!map || !city || state.loadingState !== 'ready') return;

    const hashParams = getHashParams();
    if (hashParams.center && hashParams.zoom !== undefined) {
      map.setView(hashParams.center, hashParams.zoom);
    } else {
      map.setView(city.center, city.zoom);
    }

    // Draw bounding box
    if (bboxRectRef.current) bboxRectRef.current.remove();
    const [minLon, minLat, maxLon, maxLat] = city.bbox;
    bboxRectRef.current = L.rectangle(
      [
        [minLat, minLon],
        [maxLat, maxLon],
      ],
      {
        color: '#666',
        weight: 1,
        fillOpacity: 0,
        dashArray: '4 6',
        interactive: false,
      }
    ).addTo(map);

    // Clean up old overlays
    if (sourceMarkerRef.current) {
      sourceMarkerRef.current.remove();
      sourceMarkerRef.current = null;
    }
    if (destMarkerRef.current) {
      destMarkerRef.current.remove();
      destMarkerRef.current = null;
    }
    if (isoOverlayRef.current) {
      map.removeLayer(isoOverlayRef.current);
      isoOverlayRef.current = null;
    }
    routePolylinesRef.current.forEach((p) => p.remove());
    routePolylinesRef.current = [];
  }, [state.currentCity, state.loadingState]);

  // Re-render isochrone when travel times or max time changes
  useEffect(() => {
    if (!renderIsoRef.current) return;
    renderIsoRef.current();
  }, [state.travelTimes, state.maxTimeMin]);

  // Clear destination marker and routes when unpinned
  useEffect(() => {
    if (state.pinnedNode === null) {
      clearDestination();
    }
  }, [state.pinnedNode, clearDestination]);

  // Redraw routes when the pinned hover data or the selected sample changes.
  // With a selection, show only that path; otherwise show the full Pareto set.
  useEffect(() => {
    const { hoverData, pinnedNode, selectedSampleIdx } = state;
    if (!drawRouteLayersRef.current || !hoverData || pinnedNode === null) return;
    const paths =
      selectedSampleIdx !== null
        ? [hoverData.allPaths[selectedSampleIdx]].filter(
            (p): p is HoverPath => !!p && p.segments.length > 0
          )
        : hoverData.allPaths.filter((p) => p.segments.length > 0);
    drawRouteLayersRef.current(paths);
  }, [state.selectedSampleIdx, state.hoverData, state.pinnedNode]);

  // Draw source marker when sourceNode is set externally (URL restore)
  useEffect(() => {
    const { sourceNode, sourceLatLng } = state;
    if (sourceNode === null || !sourceLatLng || !mapRef.current) return;
    if (sourceMarkerRef.current) return;
    sourceMarkerRef.current = L.marker(sourceLatLng, { title: 'Origin' }).addTo(mapRef.current);
  }, [state.sourceNode, state.sourceLatLng]);

  // Draw dest marker and routes when pinnedNode is set externally (URL restore)
  useEffect(() => {
    const { pinnedNode, pinnedLatLng, hoverData } = state;
    if (pinnedNode === null || !pinnedLatLng || !hoverData || !mapRef.current) return;
    if (destMarkerRef.current) return;
    destMarkerRef.current = L.circleMarker(pinnedLatLng, {
      radius: 6,
      color: '#fff',
      fillColor: '#4a90d9',
      fillOpacity: 1,
      weight: 2,
      pane: 'transitLines',
    }).addTo(mapRef.current);
    if (drawRouteLayersRef.current) {
      drawRouteLayersRef.current(hoverData.allPaths.filter((p) => p.segments.length > 0));
    }
  }, [state.pinnedNode, state.pinnedLatLng, state.hoverData]);

  return <div id="map" ref={mapContainerRef} />;
});

export default MapView;
