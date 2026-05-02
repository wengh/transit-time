import React, { useEffect } from 'react';
import { createPortal } from 'react-dom';
import ControlsBody from './ControlsBody';
import { LegendContent } from './Legend';
import { useAppState } from '../state/AppContext';

interface MobileSettingsSheetProps {
  onClose: () => void;
  onRunQuery: (overrides?: Record<string, any>) => void;
  onCopy: () => void;
}

export default function MobileSettingsSheet({
  onClose,
  onRunQuery,
  onCopy,
}: MobileSettingsSheetProps): React.ReactNode {
  const { state } = useAppState();

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === 'Escape') onClose();
    }
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [onClose]);

  return createPortal(
    <div
      className="fixed inset-0 z-[1200] flex flex-col justify-end"
      role="dialog"
      aria-modal="true"
    >
      <div className="absolute inset-0 bg-black/50 animate-fadeIn" onClick={onClose} />
      <div
        className="relative bg-zinc-900 text-zinc-100 rounded-t-2xl
          max-h-[85vh] overflow-y-auto px-4 pt-3
          pb-[max(env(safe-area-inset-bottom),1rem)]
          shadow-[0_-4px_16px_rgba(0,0,0,0.6)]
          animate-slideUp"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between mb-2">
          <div className="text-[14px] font-semibold truncate">
            {state.currentCity?.name ?? 'Settings'}
          </div>
          <button
            aria-label="Close settings"
            onClick={onClose}
            className="w-8 h-8 flex items-center justify-center rounded-full
              text-zinc-300 hover:bg-zinc-800 active:bg-zinc-700"
          >
            <svg
              viewBox="0 0 24 24"
              width="18"
              height="18"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        </div>

        <ControlsBody onRunQuery={onRunQuery} onCopy={onCopy} compact onChangeCity={onClose} />

        <div className="mt-3 pt-3 border-t border-zinc-700">
          <LegendContent maxMin={state.maxTimeMin} />
        </div>
      </div>
    </div>,
    document.body
  );
}
