import type { Language } from '../i18n';
import { AlefError, app, fs, path, store, type AppInfo, type DirEntry } from '@alef-tron/api';

export type { AppInfo };

export interface PreferencesResponse {
  language: Language;
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

const LANGUAGES: readonly string[] = ['ru', 'en', 'he'];
const DEFAULT_LANGUAGE: Language = 'ru';
const isLanguage = (value: unknown): value is Language => typeof value === 'string' && LANGUAGES.includes(value);

/** The folder the file manager opens first: `--root`, else the home folder (the manifest allows reading below it). */
async function rootFolder(signal?: AbortSignal): Promise<string> {
  const { parsed } = await app.args({ signal });
  const given = parsed.root;
  return typeof given === 'string' && given !== '' ? given : path.home({ signal });
}

const toEntry = (entry: DirEntry): DirectoryEntry => ({
  name: entry.name,
  path: entry.path,
  size: entry.size,
  is_dir: entry.kind === 'dir',
  is_file: entry.kind === 'file',
  is_symlink: entry.kind === 'symlink',
});

export const nativeApi = {
  info: (signal?: AbortSignal) => app.info({ signal }),
  preferences: async (signal?: AbortSignal): Promise<PreferencesResponse> => {
    const stored = await store.get<unknown>('language', { signal });
    return { language: isLanguage(stored) ? stored : DEFAULT_LANGUAGE };
  },
  setPreferences: async (language: Language): Promise<PreferencesResponse> => {
    if (!isLanguage(language)) throw new AlefError('INVALID_ARGUMENT', 'Unknown language');
    await store.set('language', language);
    await store.flush();
    return { language };
  },
  listDirectory: async (requested?: string, signal?: AbortSignal): Promise<DirectoryResponse> => {
    const root = await rootFolder(signal);
    const target = requested ?? root;
    const entries = (await fs.readDir(target, { signal })).map(toEntry);
    entries.sort((left, right) => Number(right.is_dir) - Number(left.is_dir) || left.name.localeCompare(right.name));
    const atRoot = (await path.normalize(target)) === (await path.normalize(root));
    return { root, path: target, parent: atRoot ? null : await path.dirname(target), entries };
  },
};
