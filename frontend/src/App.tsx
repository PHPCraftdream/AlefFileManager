import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { nativeApi, type HelloResponse } from './native/api';
import i18n, { languages, type Language } from './i18n';
import { listen, nativeWindow, type Unlisten, type WindowState } from './native/runtime';
import TitleBar from './TitleBar';

export default function App() {
  const { t } = useTranslation();
  const [backend, setBackend] = useState<HelloResponse | null>(null);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [windowState, setWindowState] = useState<WindowState | null>(null);
  const [windowError, setWindowError] = useState('');
  const [eventsEnabled, setEventsEnabled] = useState(true);
  const [receivedEvents, setReceivedEvents] = useState(0);
  const [lastEvent, setLastEvent] = useState<HelloResponse | null>(null);

  const reportWindowError = useCallback((failure: unknown) => {
    setWindowError(failure instanceof Error ? failure.message : String(failure));
  }, []);

  useEffect(() => {
    if (!eventsEnabled) return;
    return listen<HelloResponse>('backend.greeting', payload => {
      setLastEvent(payload);
      setReceivedEvents(count => count + 1);
    });
  }, [eventsEnabled]);

  useEffect(() => {
    const controller = new AbortController();
    let unsubscribe: Unlisten | undefined;
    void nativeWindow.watch(setWindowState, controller.signal)
      .then(release => {
        if (controller.signal.aborted) release();
        else unsubscribe = release;
      })
      .catch(failure => {
        if (!controller.signal.aborted) reportWindowError(failure);
      });
    return () => { controller.abort(); unsubscribe?.(); };
  }, [reportWindowError]);

  useEffect(() => {
    const controller = new AbortController();
    void Promise.all([nativeApi.hello(controller.signal), nativeApi.preferences(controller.signal)])
      .then(async ([hello, preferences]) => {
        if (controller.signal.aborted) return;
        await i18n.changeLanguage(preferences.language);
        if (!controller.signal.aborted) setBackend(hello);
      })
      .catch(failure => {
        if (!controller.signal.aborted) setError(failure instanceof Error ? failure.message : String(failure));
      })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, []);

  const connect = useCallback(async () => {
    setLoading(true);
    setError('');
    try {
      setBackend(await nativeApi.hello());
    } catch (failure) {
      setBackend(null);
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setLoading(false);
    }
  }, []);

  const changeLanguage = useCallback(async (language: Language) => {
    setSaving(true);
    setError('');
    try {
      const preferences = await nativeApi.setPreferences(language);
      await i18n.changeLanguage(preferences.language);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setSaving(false);
    }
  }, []);

  return (
    <div className="flex h-screen flex-col overflow-hidden bg-[#11202b] text-[#e8f2f7]">
      <TitleBar state={windowState} onError={reportWindowError} />
      <main className="flex min-h-0 flex-1 items-start justify-center overflow-auto px-6 py-8">
      <section className="w-full max-w-xl rounded-2xl border border-[#294455] bg-[#172d3c] p-8 shadow-xl">
        <div className="mb-6 flex items-center gap-4">
          <img src="/logo-32x32.png" alt="Alef" className="h-12 w-12 rounded-lg" />
          <div>
            <p className="text-sm font-medium tracking-wide text-[#87ceeb]">ALEF FILE MANAGER</p>
            <p className="text-sm text-[#a3bac9]">{t('subtitle')}</p>
          </div>
        </div>
        <h1 className="text-4xl font-semibold tracking-tight">{t('title')}</h1>
        <p className="mt-4 leading-relaxed text-[#b7cbd7]">{t('description')}</p>
        <div className="mt-6 rounded-xl bg-[#11202b] p-4" aria-live="polite">
          {loading ? <p>{t('connecting')}</p> : error ? (
            <p role="alert" className="text-[#ffb4a7]">{t('failed', { message: error })}</p>
          ) : backend ? (
            <>
              <p className="font-medium text-[#8fe4bc]">{t('connected')}</p>
              <p className="mt-1 text-sm text-[#a3bac9]">{t('identity', { pid: backend.process_id, engine: backend.engine })}</p>
            </>
          ) : null}
        </div>
        <button
          type="button"
          disabled={loading || saving}
          onClick={() => void connect()}
          className="mt-6 rounded-lg bg-[#87ceeb] px-5 py-3 font-semibold text-[#112d42] transition hover:bg-[#b5e8fb] disabled:opacity-50"
        >
          {t('callBackend')}
        </button>
        <fieldset className="mt-6 border-0 p-0">
          <legend className="mb-2">{t('language')}</legend>
          <div className="flex flex-wrap items-center gap-3">
          {languages.map(language => (
            <button
              key={language.code}
              type="button"
              disabled={loading || saving}
              aria-pressed={i18n.resolvedLanguage === language.code}
              onClick={() => void changeLanguage(language.code)}
              className={`rounded-lg border px-3 py-2 disabled:opacity-50 ${
                i18n.resolvedLanguage === language.code
                  ? 'border-[#87ceeb] bg-[#87ceeb] text-[#112d42]'
                  : 'border-[#294455] bg-[#11202b] text-[#e8f2f7]'
              }`}
            >
              {language.name}
            </button>
          ))}
          </div>
        </fieldset>
        <p className="mt-3 text-sm text-[#a3bac9]" aria-live="polite">{t(saving ? 'saving' : 'saved')}</p>
        <div className="mt-6 border-t border-[#294455] pt-4 text-sm text-[#a3bac9]">
          <p aria-live="polite">{t('eventsReceived', { count: receivedEvents })}</p>
          {lastEvent && <p className="mt-1">{lastEvent.message}</p>}
          <button type="button" onClick={() => setEventsEnabled(enabled => !enabled)}
            className="mt-2 rounded-md border border-[#294455] px-3 py-2">
            {t(eventsEnabled ? 'unsubscribeEvents' : 'subscribeEvents')}
          </button>
          {windowState && (
            <p className="mt-4" aria-live="polite">
              {t('windowState', { width: windowState.width, height: windowState.height,
                mode: t(windowState.maximized ? 'windowMaximized' : 'windowNormal') })}
            </p>
          )}
          <button type="button" aria-pressed={windowState?.decorated ?? false} disabled={!windowState}
            className="mt-3 rounded-md border border-[#294455] px-3 py-2"
            onClick={() => {
              setWindowError('');
              void nativeWindow.setDecorations(!windowState?.decorated).catch(reportWindowError);
            }}>
            {t('nativeTitlebar')}
          </button>
          <button type="button" aria-pressed={windowState?.resizable ?? true} disabled={!windowState}
            className="ms-2 mt-3 rounded-md border border-[#294455] px-3 py-2"
            onClick={() => {
              setWindowError('');
              void nativeWindow.setResizable(!windowState?.resizable).catch(reportWindowError);
            }}>
            {t(windowState?.resizable ? 'disableResize' : 'enableResize')}
          </button>
          {windowError && <p role="alert" className="mt-2 text-[#ffb4a7]">{windowError}</p>}
        </div>
      </section>
      </main>
    </div>
  );
}
