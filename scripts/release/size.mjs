// Sizes of the release binary and its archive for the job summary and the size artifact.
import { statSync } from 'node:fs';

export function sizeReport({ binaryPath, archivePath, profile }) {
  const binaryBytes = statSync(binaryPath).size;
  const archiveBytes = statSync(archivePath).size;
  return { binaryBytes, archiveBytes, profile };
}

export function formatSummary(report, platform) {
  const mib = bytes => (bytes / (1024 * 1024)).toFixed(1);
  return `| ${platform} | ${report.binaryBytes} | ${mib(report.binaryBytes)} MiB | ${report.archiveBytes} | ${mib(report.archiveBytes)} MiB | ${report.profile} |`;
}

export function writeJson(report) {
  return JSON.stringify(report, null, 2) + '\n';
}
