// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import Search from './Search';

const { findSimilarByReferenceImage, open, searchAssets } = vi.hoisted(() => ({
  findSimilarByReferenceImage: vi.fn(),
  open: vi.fn(),
  searchAssets: vi.fn(),
}));

vi.mock('@/api', () => ({
  acgCreatorPackEnabled: vi.fn().mockResolvedValue(false),
  addAssetsToDefaultSelects: vi.fn(),
  addAssetsToSelectCollection: vi.fn(),
  findAssetsForEntity: vi.fn(),
  findSimilarAssets: vi.fn(),
  findSimilarByReferenceImage,
  hiddenAssetIds: vi.fn().mockResolvedValue([]),
  listEntities: vi.fn().mockResolvedValue([]),
  listSelectCollections: vi.fn().mockResolvedValue([]),
  recentSearches: vi.fn().mockResolvedValue([]),
  searchAssets,
  setAssetHidden: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open }));
vi.mock('@/components/MediaGrid', () => ({ MediaGrid: () => null }));
vi.mock('@/components/SegmentPanel', () => ({ SegmentPanel: () => null }));

function renderSearch() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <Search />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  cleanup();
  delete (window as Window & { __SCENEWEAVER_E2E_REFERENCE_IMAGE_PATH__?: string }).__SCENEWEAVER_E2E_REFERENCE_IMAGE_PATH__;
  findSimilarByReferenceImage.mockReset();
  findSimilarByReferenceImage.mockResolvedValue([]);
  open.mockReset();
  searchAssets.mockReset();
  searchAssets.mockResolvedValue([]);
});

describe('Search page interactions', () => {
  it('uses the development-only E2E reference path without opening the system picker', async () => {
    const referencePath = 'C:\\fixtures\\reference-fixture.png';
    (window as Window & { __SCENEWEAVER_E2E_REFERENCE_IMAGE_PATH__?: string }).__SCENEWEAVER_E2E_REFERENCE_IMAGE_PATH__ = referencePath;
    const user = userEvent.setup();
    renderSearch();

    await user.click(screen.getByRole('button', { name: '参考图' }));

    await waitFor(() => {
      expect(findSimilarByReferenceImage).toHaveBeenCalled();
    });
    expect(findSimilarByReferenceImage.mock.calls[0]?.[0]).toBe(referencePath);
    expect(open).not.toHaveBeenCalled();
  });

  it('submits parsed conditions, then edits and removes a condition through the rendered chips', async () => {
    searchAssets.mockResolvedValue([]);
    const user = userEvent.setup();
    renderSearch();

    await user.type(screen.getByPlaceholderText('例如：雨夜、角色、采访'), '雨夜 优先侧脸 不要字幕');
    await user.click(screen.getByRole('button', { name: '搜索' }));

    await waitFor(() => {
      expect(searchAssets).toHaveBeenCalledWith(expect.objectContaining({
        must: ['雨夜'],
        should: ['侧脸'],
        must_not: ['字幕'],
      }), expect.anything());
    });

    await user.click(screen.getByRole('button', { name: '必须：雨夜' }));
    const conditionEditor = screen.getByLabelText('必须 条件');
    await user.clear(conditionEditor);
    await user.type(conditionEditor, '雨天');
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(searchAssets).toHaveBeenCalledWith(expect.objectContaining({
        must: ['雨天'],
        should: ['侧脸'],
        must_not: ['字幕'],
      }), expect.anything());
    });

    const mustChip = screen.getByRole('button', { name: '必须：雨天' }).parentElement;
    expect(mustChip).not.toBeNull();
    await user.click(within(mustChip!).getByTitle('移除此条件并重新搜索'));
    await waitFor(() => {
      expect(searchAssets).toHaveBeenCalledWith(expect.objectContaining({
        must: [],
        should: ['侧脸'],
        must_not: ['字幕'],
      }), expect.anything());
    });
  });

  it('replaces reference image results when submitting a keyword query', async () => {
    open.mockResolvedValue('C:\\fixtures\\reference-fixture.png');
    findSimilarByReferenceImage.mockResolvedValue([{ id: 'reference-match' }]);
    searchAssets.mockResolvedValue([]);
    const user = userEvent.setup();
    renderSearch();

    await user.click(screen.getByRole('button', { name: '参考图' }));
    expect(await screen.findByText('参考图相似结果：1 项')).toBeTruthy();

    await user.type(screen.getByPlaceholderText('例如：雨夜、角色、采访'), '雨夜');
    await user.click(screen.getByRole('button', { name: '搜索' }));

    expect(await screen.findByText('关键词结果：0 项')).toBeTruthy();
    expect(screen.queryByText('参考图相似结果：1 项')).toBeNull();
  });

  it('blocks a query whose start date is later than its end date', async () => {
    searchAssets.mockResolvedValue([]);
    const user = userEvent.setup();
    renderSearch();

    fireEvent.change(screen.getByLabelText('开始日期'), { target: { value: '2026-07-03' } });
    fireEvent.change(screen.getByLabelText('结束日期'), { target: { value: '2026-07-01' } });
    await user.type(screen.getByPlaceholderText('例如：雨夜、角色、采访'), '雨夜');

    const submit = screen.getByRole('button', { name: '搜索' });
    expect(submit).toHaveProperty('disabled', true);
    expect(screen.getByText('开始日期不能晚于结束日期')).toBeTruthy();
    await user.click(submit);
    expect(searchAssets).not.toHaveBeenCalled();
  });
});
