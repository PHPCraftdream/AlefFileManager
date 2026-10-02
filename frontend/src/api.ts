import type { Language } from './i18n';
import { invoke } from './runtime';

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
  hello: (signal?: AbortSignal) => invoke<HelloResponse>('hello', null, signal),
  preferences: (signal?: AbortSignal) => invoke<PreferencesResponse>('preferences.get', null, signal),
  setPreferences: (language: Language) => invoke<PreferencesResponse>('preferences.set', { language }),
  listDirectory: (path?: string, signal?: AbortSignal) => invoke<DirectoryResponse>(
    'directory.list', { path }, signal,
  ),
};
