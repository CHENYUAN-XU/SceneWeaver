import { invoke } from '@tauri-apps/api/core';
import { expect, it, vi } from 'vitest';
import { startScan } from './index';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

it('exposes serialized Tauri command failures as readable errors', async () => {
  vi.mocked(invoke).mockRejectedValueOnce({ Other: '素材库已有扫描任务' });
  await expect(startScan('library')).rejects.toThrow('素材库已有扫描任务');

  vi.mocked(invoke).mockRejectedValueOnce('IPC 连接失败');
  await expect(startScan('library')).rejects.toThrow('IPC 连接失败');
});
