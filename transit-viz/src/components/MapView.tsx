import React, { useCallback, useEffect, useRef } from 'react';
import L from 'leaflet';
import { useAppState } from '../state/AppContext';
import { initWebGL, renderIsochrone } from '../utils/webgl';
import { getAnyHoverData, type HoverPath } from '../utils/router';
import { ROUTE_COLORS } from '../utils/colors';
import { getHashParams, setHashParams } from '../utils/urlHash';
import { getSortedTravelTimes } from '../utils/hoverInfo';
import { resolveMapStyle, DEFAULT_MAP_STYLE } from '../utils/mapStyles';

const isTouchDevice = typeof window !== 'undefined' && ('ontouchstart' in window || navigator.maxTouchPoints > 0);

export default function MapView(): React.ReactNode {
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
  const drawRouteLayersRef = useRef<((paths: HoverPath[]) => void) | null>(null);
  const lastHoveredNodeRef = useRef<number | null>(null);
  const longPressTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const longPressStartRef = useRef<number | null>(null);
  const renderIsoRef = useRef<(() => void) | null>(null);

  // Keep current state in refs for event handlers
  const stateRef = useRef(state);
  stateRef.current = state;

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
    const map = L.map('map', { doubleClickZoom: isTouchDevice }).setView([40, -90], 4);
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

    function snapToNode(lat: number, lon: number): number | null {
      const router = stateRef.current.router;
      if (!router) return null;
      return router.snap_to_node(lat, lon) ?? null;
    }

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
      const routeColorMap: Record<string, string> = {};
      let colorIdx = 0;
      for (const { segments } of allPaths) {
        for (const seg of segments) {
          if (seg.coords.length < 2) continue;
          let color: string, dashArray: string | null, weight: number;
          if (seg.edgeType === 0) {
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
              let routeColor = s.router && seg.routeIdx < 0xffffffff
                ? s.router.route_color(seg.routeIdx)
                : '';
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
          const line = L.polyline(seg.coords, { color, weight, opacity: 1, ...(dashArray ? { dashArray } : {}), interactive: false, pane: 'transitLines' }).addTo(map);
          routePolylinesRef.current.push(line);
          // Add circle at end of transit segments to mark transfers
          if (seg.edgeType === 1) {
            const s = stateRef.current;
            if (s.nodeCoords && seg.endNodeIdx !== undefined) {
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
              }).addTo(map);
              routePolylinesRef.current.push(circle);
            }
          }
        }
      }
    }

    drawRouteLayersRef.current = drawRouteSegments;

    function showDestination(node: number, pin: boolean) {
      const s = stateRef.current;
      if (!s.router || !s.travelTimes || (!s.ssspList && !s.profile)) return;
      const tt = s.travelTimes[node];
      if (isNaN(tt) || tt < 0) {
        clearRouteOverlay();
        if (destMarkerRef.current) {
          destMarkerRef.current.remove();
          destMarkerRef.current = null;
        }
        dispatch({ type: pin ? 'UNPIN_DESTINATION' : 'CLEAR_HOVER' });
        return;
      }

      const allPaths = getAnyHoverData(s.router, s.ssspList, s.profile, node);
      const travelTimes = getSortedTravelTimes(allPaths);

      drawRouteSegments(allPaths.filter((p) => p.segments.length > 0));

      // Analytic per-node summary comes straight from the Rust profile router —
      // avoids re-aggregating from the (Pareto-filtered) `allPaths`, which no
      // longer corresponds to discrete sample counts.
      const avgTravelTime = isFinite(tt) ? tt : null;
      const reachableFraction = s.sampleCounts && s.totalSamples > 0
        ? s.sampleCounts[node] / s.totalSamples
        : null;
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

    function setSource(lat: number, lng: number) {
      const s = stateRef.current;
      if (!s.router) return;
      const node = snapToNode(lat, lng);
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

    // Desktop: double-click sets source
    let lastPinTime = 0;
    function onDblClick(e: L.LeafletMouseEvent) {
      if (!stateRef.current.router) return;
      // Prevent on mobile (handled by long press)
      if (isTouchDevice) return;
      setSource(e.latlng.lat, e.latlng.lng);
    }

    // Single click: pin/unpin destination
    function onClick(e: L.LeafletMouseEvent) {
      const s = stateRef.current;
      if (!s.router || s.sourceNode === null) return;

      // Ignore if this was part of a long press
      if (longPressStartRef.current) {
        const elapsed = Date.now() - longPressStartRef.current;
        longPressStartRef.current = null;
        if (elapsed > 400) return;
      }

      // Desktop: if already pinned, unpin on click (swallow if it's the second
      // click of a double-click so dblclick can set source cleanly).
      if (!isTouchDevice && s.pinnedNode !== null) {
        if (Date.now() - lastPinTime < 300) return;
        dispatch({ type: 'UNPIN_DESTINATION' });
        return;
      }

      // Otherwise (or on mobile regardless of pin state): pin the clicked position.
      const node = snapToNode(e.latlng.lat, e.latlng.lng);
      if (node !== null) {
        lastPinTime = Date.now();
        showDestination(node, true);
      }
    }

    // Hover: show route (desktop, no pinned dest)
    function onMouseMove(e: L.LeafletMouseEvent) {
      const s = stateRef.current;
      if (!s.router || !s.travelTimes || (!s.ssspList && !s.profile) || s.pinnedNode !== null) return;

      const node = snapToNode(e.latlng.lat, e.latlng.lng);
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

    // Mobile: long press to set source
    function onTouchStart(e: TouchEvent) {
      if (e.touches.length !== 1) return;
      const touch = e.touches[0];
      longPressStartRef.current = Date.now();
      longPressTimerRef.current = setTimeout(() => {
        if (!stateRef.current.router) return;
        const latLng = map.containerPointToLatLng(new L.Point(touch.clientX, touch.clientY));
        setSource(latLng.lat, latLng.lng);
        longPressStartRef.current = null;
      }, 600);
    }

    function onTouchEnd() {
      if (longPressTimerRef.current) {
        clearTimeout(longPressTimerRef.current);
        longPressTimerRef.current = null;
      }
    }

    function onTouchMove() {
      if (longPressTimerRef.current) {
        clearTimeout(longPressTimerRef.current);
        longPressTimerRef.current = null;
      }
      longPressStartRef.current = null;
    }

    // Re-render isochrone on map move/zoom
    function renderIso() {
      const s = stateRef.current;
      if (!s.travelTimes || !s.nodeCoords || !map) return;
      if (!glStateRef.current) {
        glStateRef.current = initWebGL();
      }
      if (!glStateRef.current) return;
      const result = renderIsochrone(glStateRef.current, map, s.travelTimes, s.nodeCoords, s.maxTimeMin * 60, L, s.sampleCounts, s.totalSamples);
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
              const events: Record<string, (e: any) => void> = { zoom: this._reset, viewreset: this._reset };
              if (this._zoomAnimated) {
                events.zoomanim = this._animateZoom;
              }
              return events;
            },
            _animateZoom(e: any) {
              const m: L.Map = this._map;
              const scale = m.getZoomScale(e.zoom);
              const offset = (m as any)._latLngBoundsToNewLayerBounds(this._isoBounds, e.zoom, e.center).min;
              L.DomUtil.setTransform(this._isoCanvas, offset, scale);
            },
            _reset() {
              const m: L.Map = this._map;
              if (!m) return;
              const topLeft = m.latLngToLayerPoint(this._isoBounds.getNorthWest());
              const bottomRight = m.latLngToLayerPoint(this._isoBounds.getSouthEast());
              L.DomUtil.setTransform(this._isoCanvas, topLeft, 1);
              this._isoCanvas.style.width = (bottomRight.x - topLeft.x) + 'px';
              this._isoCanvas.style.height = (bottomRight.y - topLeft.y) + 'px';
            },
          });
          isoOverlayRef.current = (new (CanvasLayer as any)()).addTo(map);
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

    // Touch events on the map container
    const container = map.getContainer();
    const touchStartHandler = (e: TouchEvent) => onTouchStart(e);
    container.addEventListener('touchstart', touchStartHandler, { passive: true });
    container.addEventListener('touchend', onTouchEnd, { passive: true });
    container.addEventListener('touchmove', onTouchMove, { passive: true });

    return () => {
      map.off('dblclick', onDblClick);
      map.off('click', onClick);
      map.off('mousemove', onMouseMove);
      map.off('mouseout', onMouseOut);
      map.off('moveend', onMoveEnd);
      map.off('zoomend', onMoveEnd);
      container.removeEventListener('touchstart', touchStartHandler);
      container.removeEventListener('touchend', onTouchEnd);
      container.removeEventListener('touchmove', onTouchMove);
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
    bboxRectRef.current = L.rectangle([[minLat, minLon], [maxLat, maxLon]], {
      color: '#666',
      weight: 1,
      fillOpacity: 0,
      dashArray: '4 6',
      interactive: false,
    }).addTo(map);

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

  // Redraw routes when selected sample changes (chart hover/click)
  useEffect(() => {
    const { hoverData, selectedSampleIdx, pinnedNode } = state;
    if (!drawRouteLayersRef.current || !hoverData || pinnedNode === null) return;
    const paths = selectedSampleIdx !== null
      ? [hoverData.allPaths[selectedSampleIdx]].filter((p): p is HoverPath => !!p && p.segments.length > 0)
      : hoverData.allPaths.filter(p => p.segments.length > 0);
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
}
