import React from 'react';
import { useAppState } from '../state/AppContext';
import type { HoverPath } from '../utils/router';
import { routeColorFor } from '../utils/colors';

interface PathSegmentListProps {
  path: HoverPath;
}

export default function PathSegmentList({ path }: PathSegmentListProps): React.ReactNode {
  const { state } = useAppState();

  return (
    <div
      className="border-b border-zinc-800 dark:border-zinc-800
        [@media(prefers-color-scheme:light)]:border-zinc-200
        pb-1.5 mb-0.5"
    >
      {path.segments.map((seg, si) => (
        <div key={si}>
          {seg.edgeType === 0 ? (
            <div
              className="text-[12px] text-zinc-500 dark:text-zinc-500
                [@media(prefers-color-scheme:light)]:text-zinc-500 py-0.5"
            >
              Walk {(seg.duration / 60).toFixed(1)} min
            </div>
          ) : (
            <>
              {seg.waitTime > 0 && (
                <div
                  className="text-[11px] text-zinc-600 dark:text-zinc-600
                    [@media(prefers-color-scheme:light)]:text-zinc-500
                    py-px italic"
                >
                  Wait {(seg.waitTime / 60).toFixed(1)} min
                </div>
              )}
              <div
                className="text-[12px] py-0.5 text-zinc-100 dark:text-zinc-100
                  [@media(prefers-color-scheme:light)]:text-zinc-900"
              >
                <span
                  className="inline-block w-2.5 h-2.5 rounded-full mr-1.5 align-middle"
                  style={{ background: routeColorFor(seg.routeIdx, state.routeColors) }}
                />
                <b>{seg.routeName || 'Transit'}</b>
                {seg.startStopName && seg.endStopName
                  ? ` · ${seg.startStopName} → ${seg.endStopName}`
                  : ''}{' '}
                {(seg.duration / 60).toFixed(1)} min
              </div>
            </>
          )}
        </div>
      ))}
    </div>
  );
}
