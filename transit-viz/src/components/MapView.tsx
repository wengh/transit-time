import React, { useCallback, useEffect, useRef } from 'react';
import L from 'leaflet';
import { useAppState } from '../state/AppContext';
import { initWebGL, renderIsochrone } from '../utils/webgl';
import { getHoverData, type HoverPath } from '../utils/router';
import { ROUTE_COLORS, hexToRgb } from '../utils/colors';

const isTouchDevice = typeof window !== 'undefined' && ('ontouchstart' in window || navigator.maxTouchPoints > 0);

export default function MapView(): React.ReactNode {
  const { state, dispatch } = useAppState();
  const mapRef = useRef<L.Map | null>(null);
  const mapContainerRef = useRef<HTMLDivElement>(null);
  const glStateRef = useRef<ReturnType<typeof initWebGL> | null>(null);
  const isoOverlayRef = useRef<L.ImageOverlay | null>(null);
  const sourceMarkerRef = useRef<L.Marker | null>(null);
  const destMarkerRef = useRef<L.CircleMarker | null>(null);
  const routePolylinesRef = useRef<L.Polyline[]>([]);
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
    L.tileLayer('https://{s}.basemaps.cartocdn.com/dark_nolabels/{z}/{x}/{y}{r}.png', {
      attribution: '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> &copy; <a href="https://carto.com/">CARTO</a>',
      maxZoom: 20,
      subdomains: 'abcd',
      crossOrigin: true,
    }).addTo(map);
    mapRef.current = map;

    return () => {
      map.remove();
      mapRef.current = null;
    };
  }, []);

  // Set up map event handlers
  useEffect(() => {
    const map = mapRef.current as L.Map;
    if (!map) return;

    function snapToNode(lat: number, lon: number): number | null {
      const router = stateRef.current.router;
      if (!router) return null;
      return router.snap_to_node(lat, lon);
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
              let routeColor: string | null = null;
              // Try to get actual color from GTFS
              const s = stateRef.current;
              if (s.router && seg.routeIdx < 0xffffffff) {
                const hexColor = s.router.route_color(seg.routeIdx);
                if (hexColor) {
                  const rgb = hexToRgb(hexColor);
                  if (rgb) {
                    // Ensure color has enough brightness for dark background
                    const brightness = (rgb[0] * 299 + rgb[1] * 587 + rgb[2] * 114) / 1000;
                    const minBrightness = 100;
                    const maxBrightness = 220;

                    if (brightness < minBrightness) {
                      // Too dark, lighten it
                      const scale = minBrightness / brightness;
                      routeColor = `rgb(${Math.min(255, Math.round(rgb[0] * scale))}, ${Math.min(255, Math.round(rgb[1] * scale))}, ${Math.min(255, Math.round(rgb[2] * scale))})`;
                    } else if (brightness > maxBrightness) {
                      // Too light, darken it
                      const scale = maxBrightness / brightness;
                      routeColor = `rgb(${Math.round(rgb[0] * scale)}, ${Math.round(rgb[1] * scale)}, ${Math.round(rgb[2] * scale)})`;
                    } else {
                      routeColor = hexColor;
                    }
                  }
                }
              }
              // Fall back to palette colors if GTFS color unavailable
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
          const line = L.polyline(seg.coords, { color, weight, opacity: 1, ...(dashArray ? { dashArray } : {}), interactive: false }).addTo(map);
          routePolylinesRef.current.push(line);
        }
      }
    }

    function showDestination(node: number, pin: boolean) {
      const s = stateRef.current;
      if (!s.router || !s.travelTimes || !s.ssspList) return;
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

      const allPaths = getHoverData(s.router, s.ssspList, node);
      const travelTimes = allPaths
        .map((p) => p.totalTime)
        .filter((t): t is number => t !== null && isFinite(t))
        .sort((a, b) => a - b);

      drawRouteSegments(allPaths.filter((p) => p.segments.length > 0));

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
          }).addTo(map);
        }
        dispatch({ type: 'PIN_DESTINATION', node, latLng, hoverData: { allPaths, travelTimes } });
      } else {
        dispatch({ type: 'SET_HOVER_DATA', hoverData: { allPaths, travelTimes } });
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
    let clickTimer: ReturnType<typeof setTimeout> | null = null;
    function onDblClick(e: L.LeafletMouseEvent) {
      if (!stateRef.current.router) return;
      // Prevent on mobile (handled by long press)
      if (isTouchDevice) return;
      // Cancel pending single-click
      if (clickTimer) {
        clearTimeout(clickTimer);
        clickTimer = null;
      }
      setSource(e.latlng.lat, e.latlng.lng);
    }

    // Single click: pin/unpin destination (delayed on desktop to distinguish from dblclick)
    function onClick(e: L.LeafletMouseEvent) {
      const s = stateRef.current;
      if (!s.router || s.sourceNode === null) return;

      // Ignore if this was part of a long press
      if (longPressStartRef.current) {
        const elapsed = Date.now() - longPressStartRef.current;
        longPressStartRef.current = null;
        if (elapsed > 400) return;
      }

      function doClick() {
        const s = stateRef.current;
        if (s.pinnedNode !== null) {
          dispatch({ type: 'UNPIN_DESTINATION' });
        } else {
          const node = snapToNode(e.latlng.lat, e.latlng.lng);
          if (node !== null) {
            showDestination(node, true);
          }
        }
      }

      if (isTouchDevice) {
        doClick();
      } else {
        // Delay to allow dblclick to cancel
        if (clickTimer) clearTimeout(clickTimer);
        clickTimer = setTimeout(() => {
          clickTimer = null;
          doClick();
        }, 250);
      }
    }

    // Hover: show route (desktop, no pinned dest)
    function onMouseMove(e: L.LeafletMouseEvent) {
      const s = stateRef.current;
      if (!s.router || !s.travelTimes || !s.ssspList || s.pinnedNode !== null) return;

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
      const result = renderIsochrone(glStateRef.current, map, s.travelTimes, s.nodeCoords, s.maxTimeMin * 60, L);
      if (result) {
        const oldOverlay = isoOverlayRef.current;
        isoOverlayRef.current = L.imageOverlay(result.dataUrl, result.renderBounds, {
          opacity: 1,
          interactive: false,
          zIndex: 500,
        }).addTo(map);
        if (oldOverlay) map.removeLayer(oldOverlay);
      }
    }

    // Store render function in ref for external trigger
    renderIsoRef.current = renderIso;

    function onMoveEnd() {
      renderIso();
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
      if (clickTimer) clearTimeout(clickTimer);
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

    // Need to look up city details from state
    const cityData = state as any; // Get full city data from state
    if (cityData.currentCity) {
      map.setView(cityData.currentCity.center, cityData.currentCity.zoom);
    }

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

  return <div id="map" ref={mapContainerRef} />;
}
