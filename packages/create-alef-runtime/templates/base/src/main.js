import { app } from '@alef-runtime/api/app';
import { permissionExample } from './permissions.js';

const info = document.querySelector('#app-info');
try {
  info.textContent = JSON.stringify(await app.info(), null, 2);
} catch (error) {
  info.textContent = `Application information unavailable: ${error.code ?? error.message}`;
}

const button = document.querySelector('#permissions');
const result = document.querySelector('#permission-result');
button.addEventListener('click', async () => {
  button.disabled = true;
  try {
    result.textContent = (await permissionExample()).join('\n');
  } catch (error) {
    result.textContent = `Example unavailable: ${error.code ?? error.message}`;
  } finally {
    button.disabled = false;
  }
});
