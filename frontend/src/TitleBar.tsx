import type { PointerEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { nativeWindow, type WindowState } from './runtime';

export default function TitleBar({ state, onError }: {
  state: WindowState | null;
  onError: (error: unknown) => void;
}) {
  const { t } = useTranslation();
  if (state?.decorated) return null;

  const drag = (event: PointerEvent<HTMLButtonElement>) => {
    if (event.button !== 0) return;
    event.preventDefault();
    void nativeWindow.startDrag().catch(onError);
  };

  return (
      <header className="flex h-10 shrink-0 items-stretch border-b border-[#294455] bg-[#172d3c]" dir="ltr">
        <button
          type="button"
          aria-label={t('moveWindow')}
          onPointerDown={drag}
          onDoubleClick={() => void nativeWindow.toggleMaximize().catch(onError)}
          onClick={event => {
            if (event.detail === 0) void nativeWindow.toggleMaximize().catch(onError);
          }}
          className="flex min-w-0 flex-1 select-none items-center gap-2 px-4 text-start text-sm text-[#b7cbd7]"
        >
          <img src="/logo-32x32.png" alt="" className="h-5 w-5 rounded-sm" />
          <span className="truncate">{state?.title ?? 'Alef File Manager'}</span>
        </button>
        <button type="button" aria-label={t('minimizeWindow')} disabled={!state}
          onClick={() => void nativeWindow.minimize().catch(onError)}
          className="flex w-12 items-center justify-center hover:bg-[#294455]">
          <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16"><path d="M3 11h10" fill="none" stroke="#e8f2f7" /></svg>
        </button>
        <button type="button" aria-label={t(state?.maximized ? 'restoreWindow' : 'maximizeWindow')} disabled={!state}
          onClick={() => void (state?.maximized ? nativeWindow.restore() : nativeWindow.maximize()).catch(onError)}
          className="flex w-12 items-center justify-center hover:bg-[#294455]">
          <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16">
            {state?.maximized
              ? <><rect x="5" y="3" width="8" height="8" fill="none" stroke="#e8f2f7" /><rect x="3" y="5" width="8" height="8" fill="#172d3c" stroke="#e8f2f7" /></>
              : <path d="M3 3h10v10H3z" fill="none" stroke="#e8f2f7" />}
          </svg>
        </button>
        <button type="button" aria-label={t('closeWindow')} disabled={!state}
          onClick={() => void nativeWindow.close().catch(onError)}
          className="flex w-12 items-center justify-center hover:bg-[#c94242] hover:text-white">
          <svg aria-hidden="true" width="16" height="16" viewBox="0 0 16 16"><path d="m4 4 8 8m0-8-8 8" fill="none" stroke="#e8f2f7" /></svg>
        </button>
      </header>
  );
}
