import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';

export type Language = 'ru' | 'en' | 'he';
export const languages: readonly { code: Language; name: string }[] = [
  { code: 'ru', name: 'Русский' },
  { code: 'en', name: 'English' },
  { code: 'he', name: 'עברית' },
];

const resources = {
  ru: { translation: {
    title: 'Привет, мир!',
    subtitle: 'Servo · Rust · React',
    description: 'React и TypeScript работают внутри встроенного Servo. Rsbuild собирает frontend, Tailwind оформляет его, а Rust выполняет асинхронный backend.',
    connecting: 'Подключение к Rust…',
    connected: 'Rust backend подключён',
    identity: 'Процесс: {{pid}} · {{engine}}',
    callBackend: 'Вызвать Rust backend',
    language: 'Язык',
    saved: 'Настройка языка сохраняется в Fjall',
    saving: 'Сохранение в Fjall…',
    failed: 'Ошибка: {{message}}',
    moveWindow: 'Переместить окно; двойной клик — развернуть',
    minimizeWindow: 'Свернуть окно',
    maximizeWindow: 'Развернуть окно',
    restoreWindow: 'Восстановить окно',
    closeWindow: 'Закрыть окно',
    nativeTitlebar: 'Показывать системный заголовок',
    eventsReceived: 'Получено событий из Rust: {{count}}',
    subscribeEvents: 'Подписаться на события',
    unsubscribeEvents: 'Отписаться от событий',
    windowState: 'Окно: {{width}} × {{height}} · {{mode}}',
    windowNormal: 'обычное',
    windowMaximized: 'развёрнуто',
    enableResize: 'Включить изменение размера',
    disableResize: 'Выключить изменение размера',
  } },
  en: { translation: {
    title: 'Hello world!',
    subtitle: 'Servo · Rust · React',
    description: 'React and TypeScript run inside embedded Servo. Rsbuild compiles the frontend, Tailwind styles it, and Rust runs the asynchronous backend.',
    connecting: 'Connecting to Rust…',
    connected: 'Rust backend connected',
    identity: 'Process: {{pid}} · {{engine}}',
    callBackend: 'Call Rust backend',
    language: 'Language',
    saved: 'Language preference is stored in Fjall',
    saving: 'Saving to Fjall…',
    failed: 'Error: {{message}}',
    moveWindow: 'Move window; double click to maximize',
    minimizeWindow: 'Minimize window',
    maximizeWindow: 'Maximize window',
    restoreWindow: 'Restore window',
    closeWindow: 'Close window',
    nativeTitlebar: 'Show system titlebar',
    eventsReceived: 'Events received from Rust: {{count}}',
    subscribeEvents: 'Subscribe to events',
    unsubscribeEvents: 'Unsubscribe from events',
    windowState: 'Window: {{width}} × {{height}} · {{mode}}',
    windowNormal: 'normal',
    windowMaximized: 'maximized',
    enableResize: 'Enable resizing',
    disableResize: 'Disable resizing',
  } },
  he: { translation: {
    title: 'שלום עולם!',
    subtitle: 'Servo · Rust · React',
    description: 'React ו־TypeScript פועלים בתוך Servo. Rsbuild בונה את הממשק, Tailwind מעצב אותו, ו־Rust מפעיל את השרת האסינכרוני.',
    connecting: 'מתחבר ל־Rust…',
    connected: 'שרת Rust מחובר',
    identity: 'תהליך: {{pid}} · {{engine}}',
    callBackend: 'קריאה לשרת Rust',
    language: 'שפה',
    saved: 'העדפת השפה נשמרת ב־Fjall',
    saving: 'שומר ב־Fjall…',
    failed: 'שגיאה: {{message}}',
    moveWindow: 'הזזת חלון; לחיצה כפולה להגדלה',
    minimizeWindow: 'מזעור חלון',
    maximizeWindow: 'הגדלת חלון',
    restoreWindow: 'שחזור חלון',
    closeWindow: 'סגירת חלון',
    nativeTitlebar: 'הצגת כותרת המערכת',
    eventsReceived: 'אירועים שהתקבלו מ־Rust: {{count}}',
    subscribeEvents: 'הרשמה לאירועים',
    unsubscribeEvents: 'ביטול הרשמה לאירועים',
    windowState: 'חלון: {{width}} × {{height}} · {{mode}}',
    windowNormal: 'רגיל',
    windowMaximized: 'מוגדל',
    enableResize: 'הפעלת שינוי גודל',
    disableResize: 'ביטול שינוי גודל',
  } },
};

i18n.on('languageChanged', language => {
  document.documentElement.lang = language;
  document.documentElement.dir = language === 'he' ? 'rtl' : 'ltr';
});

export const i18nReady = i18n.use(initReactI18next).init({
  resources,
  lng: 'ru',
  fallbackLng: 'ru',
  supportedLngs: ['ru', 'en', 'he'],
  interpolation: { escapeValue: false },
});

export default i18n;
