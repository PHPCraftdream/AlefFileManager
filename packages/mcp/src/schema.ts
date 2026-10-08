// SPDX-License-Identifier: MIT OR Apache-2.0
import { object } from './types.ts';
export type Schema = boolean | {
  type?: string | string[]; properties?: Record<string, Schema>; required?: string[];
  enum?: unknown[]; const?: unknown; items?: Schema; additionalProperties?: Schema;
  minimum?: number; maximum?: number; minLength?: number; maxLength?: number; pattern?: string;
  allOf?: Schema[]; anyOf?: Schema[]; oneOf?: Schema[]; not?: Schema;
  $ref?: string; $defs?: Record<string, Schema>; description?: string; title?: string;
};
function equal(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (Array.isArray(a) && Array.isArray(b)) return a.length === b.length && a.every((item, index) => equal(item, b[index]));
  if (object(a) && object(b)) return Object.keys(a).length === Object.keys(b).length && Object.keys(a).every(key => Object.hasOwn(b, key) && equal(a[key], b[key]));
  return false;
}
function matches(type: string, value: unknown): boolean {
  switch (type) {
    case 'null': return value === null;
    case 'object': return object(value);
    case 'array': return Array.isArray(value);
    case 'integer': return typeof value === 'number' && Number.isFinite(value) && Number.isInteger(value);
    case 'number': return typeof value === 'number' && Number.isFinite(value);
    case 'string': return typeof value === 'string';
    case 'boolean': return typeof value === 'boolean';
    default: throw new Error(`Unsupported schema type: ${type}.`);
  }
}
/** Returns validation errors for the documented subset, not a full JSON Schema implementation. */
export function validate(schema: Schema, value: unknown): string[] {
  const walk = (rule: Schema, input: unknown, path: string, depth: number): string[] => {
    if (depth > 64) return [`${path}: schema recursion limit exceeded.`];
    if (rule === true) return [];
    if (rule === false) return [`${path}: value is forbidden.`];
    if (!object(rule)) throw new Error('Invalid schema.');
    const issues: string[] = [];
    const fail = (text: string) => issues.push(`${path}: ${text}`);
    if (rule.$ref !== undefined) {
      if (!rule.$ref.startsWith('#/$defs/')) throw new Error('Only local #/$defs references are supported.');
      let target: unknown = schema;
      for (const part of rule.$ref.slice(2).split('/')) {
        const key = part.replaceAll('~1', '/').replaceAll('~0', '~');
        target = object(target) && Object.hasOwn(target, key) ? target[key] : undefined;
      }
      if (target === undefined) throw new Error(`Unresolved schema reference: ${rule.$ref}.`);
      issues.push(...walk(target as Schema, input, path, depth + 1));
    }
    if (rule.type !== undefined && !(Array.isArray(rule.type) ? rule.type : [rule.type]).some(type => matches(type, input))) fail('type does not match.');
    if (rule.enum && !rule.enum.some(item => equal(item, input))) fail('value is not in enum.');
    if (Object.hasOwn(rule, 'const') && !equal(rule.const, input)) fail('value does not match const.');
    if (typeof input === 'number') {
      if (rule.minimum !== undefined && input < rule.minimum) fail('value is below minimum.');
      if (rule.maximum !== undefined && input > rule.maximum) fail('value is above maximum.');
    }
    if (typeof input === 'string') {
      const length = Array.from(input).length;
      if (rule.minLength !== undefined && length < rule.minLength) fail('string is too short.');
      if (rule.maxLength !== undefined && length > rule.maxLength) fail('string is too long.');
      if (rule.pattern !== undefined && !new RegExp(rule.pattern, 'u').test(input)) fail('pattern does not match.');
    }
    if (Array.isArray(input) && rule.items !== undefined) input.forEach((item, index) => issues.push(...walk(rule.items!, item, `${path}[${index}]`, depth + 1)));
    if (object(input)) {
      for (const key of rule.required ?? []) if (!Object.hasOwn(input, key)) fail(`required property ${key} is missing.`);
      for (const [key, item] of Object.entries(input)) {
        const property = rule.properties && Object.hasOwn(rule.properties, key) ? rule.properties[key] : rule.additionalProperties;
        if (property !== undefined) issues.push(...walk(property, item, `${path}.${key}`, depth + 1));
      }
    }
    for (const part of rule.allOf ?? []) issues.push(...walk(part, input, path, depth + 1));
    if (rule.anyOf && !rule.anyOf.some(part => walk(part, input, path, depth + 1).length === 0)) fail('no anyOf branch matches.');
    if (rule.oneOf && rule.oneOf.filter(part => walk(part, input, path, depth + 1).length === 0).length !== 1) fail('exactly one oneOf branch must match.');
    if (rule.not !== undefined && walk(rule.not, input, path, depth + 1).length === 0) fail('not schema matches.');
    return issues;
  };
  return walk(schema, value, '$', 0);
}
