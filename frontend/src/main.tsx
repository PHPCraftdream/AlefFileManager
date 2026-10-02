import { createRoot } from 'react-dom/client';
import App from './App';
import { i18nReady } from './i18n';
import './styles.css';

const root = document.getElementById('root');
if (!root) throw new Error('Application mount point is missing.');
await i18nReady;
createRoot(root).render(<App />);
