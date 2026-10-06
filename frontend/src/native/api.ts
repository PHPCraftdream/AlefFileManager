import type { Language } from '../i18n';
import { call } from '@alef-tron/api';

export interface PreferencesResponse {
  language: Language;
}

export interface HelloResponse {
  message: string;
  process_id: number;
  engine: string;
}

export interface DirectoryEntry {
  name: string;
  path: string;
  size: number;
  is_dir: boolean;
  is_file: boolean;
  is_symlink: boolean;
}

export interface DirectoryResponse {
  root: string;
  path: string;
  parent: string | null;
  entries: DirectoryEntry[];
}


export const nativeApi = {
  hello: (signal?: AbortSignal) => call<HelloResponse>('app.hello', null, { signal }),
  preferences: (signal?: AbortSignal) => call<PreferencesResponse>('preferences.get', null, { signal }),
  setPreferences: (language: Language) => call<PreferencesResponse>('preferences.set', { language }),
  listDirectory: (path?: string, signal?: AbortSignal) => call<DirectoryResponse>(
    'directory.list', { path }, { signal },
  ),
};
