import { addAssetAcgTag, listAssetAcgTags, openAsset, removeAssetAcgTag, revealAssetInFolder, copyAssetRelativePath, copyToClipboard, detectAssetShots, favoriteAssetIds, toggleFavorite as persistFavorite, addToDefaultSelects } from '@/api';
import { formatAssetDate, formatBytes, formatDuration } from '@/lib/mediaFormat';
import { useEffect, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import type { Asset } from '@/types';

interface MediaGridProps {
  assets: Asset[];
  onViewSegments?: (assetId: string, matchingSegmentIds?: string[]) => void;
  onViewText?: (asset: Asset) => void;
  onFindSimilar?: (assetId: string) => void;
  onEntityFeedback?: (assetId: string, isPositive: boolean) => void;
  explanations?: Record<string, { reasons: string[]; unmet: string[] }>;
  matchingSegments?: Record<string, string[]>;
  selectedAssetIds?: Set<string>;
  onToggleSelection?: (assetId: string) => void;
  onHide?: (assetId: string) => void;
}

export function MediaGrid({ assets, onViewSegments, onViewText, onFindSimilar, onEntityFeedback, explanations, matchingSegments, selectedAssetIds, onToggleSelection, onHide }: MediaGridProps) {
  const favorites = useQuery({ queryKey: ['favorites'], queryFn: favoriteAssetIds });
  if (assets.length === 0) {
    return (
      <div className="rounded-xl border border-dashed border-neutral-300 p-8 text-center text-neutral-500 dark:border-neutral-700">
        暂无媒体文件
      </div>
    );
  }

  return (
    <div className="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6">
      {assets.map((asset) => (
        <MediaCard key={asset.id} asset={asset} isFavorite={favorites.data?.includes(asset.id) ?? false} onViewSegments={onViewSegments} onViewText={onViewText} onFindSimilar={onFindSimilar} onEntityFeedback={onEntityFeedback} onHide={onHide} explanation={explanations?.[asset.id]} matchingSegmentIds={matchingSegments?.[asset.id]} batchSelected={selectedAssetIds?.has(asset.id) ?? false} onToggleSelection={onToggleSelection} />
      ))}
    </div>
  );
}

function MediaCard({ asset, isFavorite, onViewSegments, onViewText, onFindSimilar, onEntityFeedback, onHide, explanation, matchingSegmentIds, batchSelected, onToggleSelection }: { asset: Asset; isFavorite: boolean; onViewSegments?: (assetId: string, matchingSegmentIds?: string[]) => void; onViewText?: (asset: Asset) => void; onFindSimilar?: (assetId: string) => void; onEntityFeedback?: (assetId: string, isPositive: boolean) => void; onHide?: (assetId: string) => void; explanation?: { reasons: string[]; unmet: string[] }; matchingSegmentIds?: string[]; batchSelected: boolean; onToggleSelection?: (assetId: string) => void }) {
  const [isDetecting, setIsDetecting] = useState(false);
  const [segmentCount, setSegmentCount] = useState<number | null>(null);
  const [detectionError, setDetectionError] = useState<string | null>(null);
  const [favorite, setFavorite] = useState(isFavorite);
  const [selected, setSelected] = useState(false);
  const [tagError, setTagError] = useState<string | null>(null);
  const tags = useQuery({ queryKey: ['assetAcgTags', asset.id], queryFn: () => listAssetAcgTags(asset.id) });
  useEffect(() => setFavorite(isFavorite), [isFavorite]);

  const detectShots = async () => {
    setIsDetecting(true);
    setDetectionError(null);
    try {
      const segments = await detectAssetShots(asset.id);
      setSegmentCount(segments.length);
    } catch (error) {
      setDetectionError(error instanceof Error ? error.message : '镜头检测失败');
    } finally {
      setIsDetecting(false);
    }
  };

  const toggleFavorite = async () => {
    setFavorite(await persistFavorite(asset.id));
  };
  const addToSelects = async () => {
    await addToDefaultSelects(asset.id);
    setSelected(true);
  };
  const addAcgTag = async () => {
    const value = window.prompt('添加 ACG 本地标签（例如：战斗、游戏 UI、侧脸、雨夜）');
    if (!value?.trim()) return;
    setTagError(null);
    try {
      await addAssetAcgTag(asset.id, value);
      await tags.refetch();
    } catch (error) {
      setTagError(error instanceof Error ? error.message : '标签保存失败');
    }
  };
  const removeAcgTag = async (value: string) => {
    setTagError(null);
    try {
      await removeAssetAcgTag(asset.id, value);
      await tags.refetch();
    } catch (error) {
      setTagError(error instanceof Error ? error.message : '标签移除失败');
    }
  };
  return (
    <div data-asset-id={asset.id} className="group relative overflow-hidden rounded-xl border border-neutral-200 bg-white dark:border-neutral-800 dark:bg-neutral-950">
      {onToggleSelection && <label className="absolute left-2 top-2 z-10 rounded bg-white/90 px-1.5 py-1 text-xs shadow dark:bg-neutral-950/90"><input type="checkbox" checked={batchSelected} onChange={() => onToggleSelection(asset.id)} /> <span className="sr-only">选择素材</span></label>}
      <div className="flex aspect-video items-center justify-center bg-neutral-100 dark:bg-neutral-900">
        {asset.thumbnail_data_url ? (
          <img
            src={asset.thumbnail_data_url}
            alt={asset.file_name}
            className="h-full w-full object-cover"
            loading="lazy"
          />
        ) : asset.media_type === 'video' ? (
          <span className="text-3xl">🎬</span>
        ) : asset.media_type === 'image' ? (
          <span className="text-3xl">🖼️</span>
        ) : (
          <span className="text-3xl">🔊</span>
        )}
      </div>
      <div className="p-2">
        <p className="truncate text-xs font-medium" title={asset.file_name}>
          {asset.file_name}
        </p>
        <p className="text-xs text-neutral-500">
          {formatDuration(asset.duration_ms)} · {formatBytes(asset.size_bytes)}
        </p>
        <p className="truncate text-[10px] text-neutral-500" title={formatAssetDate(asset)}>
          {formatAssetDate(asset)}
        </p>
        {!!tags.data?.length && <div className="mt-1 flex flex-wrap gap-1">{tags.data.map((tag) => <span key={tag} className="inline-flex items-center rounded bg-violet-100 px-1.5 py-0.5 text-[10px] text-violet-700 dark:bg-violet-950 dark:text-violet-300">{tag}<button onClick={() => removeAcgTag(tag)} title={`移除标签：${tag}`} className="ml-1 leading-none">×</button></span>)}</div>}
        {asset.media_type === 'video' && segmentCount !== null && (
          <p className="text-xs text-neutral-500">已生成 {segmentCount} 个片段</p>
        )}
        {!!matchingSegmentIds?.length && <p className="text-xs text-brand-600 dark:text-brand-300">已定位 {matchingSegmentIds.length} 个匹配片段</p>}
        {detectionError && <p className="text-xs text-red-600">{detectionError}</p>}
        {tagError && <p className="text-xs text-red-600">{tagError}</p>}
        {explanation && <p className="mt-1 line-clamp-2 text-xs text-neutral-500" title={`${explanation.reasons.join('；')}${explanation.unmet.length ? `；未命中偏好：${explanation.unmet.join('、')}` : ''}`}>{explanation.reasons.join('；')}{explanation.unmet.length ? ` · 未命中偏好：${explanation.unmet.join('、')}` : ''}</p>}
      </div>
      <div className="absolute inset-x-0 bottom-0 flex justify-around bg-white/90 p-1 opacity-0 transition-opacity group-hover:opacity-100 dark:bg-neutral-950/90">
        <ActionButton onClick={() => openAsset(asset.id)} title="打开原文件">
          打开
        </ActionButton>
        <ActionButton onClick={() => revealAssetInFolder(asset.id)} title="打开所在文件夹">
          文件夹
        </ActionButton>
        <ActionButton onClick={() => copyToClipboard(asset.file_path)} title="复制路径">
          复制
        </ActionButton>
        <ActionButton onClick={() => copyAssetRelativePath(asset.id)} title="复制相对素材库根目录的路径">
          相对路径
        </ActionButton>
        <ActionButton onClick={toggleFavorite} title={favorite ? '取消收藏' : '收藏'}>
          {favorite ? '★' : '☆'}
        </ActionButton>
        <ActionButton onClick={addToSelects} title="加入我的选片">
          {selected ? '已选' : '选片'}
        </ActionButton>
        <ActionButton onClick={addAcgTag} title="添加本地 ACG 标签">标签</ActionButton>
        {onFindSimilar && <ActionButton onClick={() => onFindSimilar(asset.id)} title="查找视觉相似素材">相似</ActionButton>}
        {onViewText && <ActionButton onClick={() => onViewText(asset)} title="查看和分析本地文字">文字</ActionButton>}
        {onHide && <ActionButton onClick={() => onHide(asset.id)} title="在搜索结果中隐藏此素材">隐藏</ActionButton>}
        {onEntityFeedback && <><ActionButton onClick={() => onEntityFeedback(asset.id, true)} title="标记为该实体并作为正样本">是实体</ActionButton><ActionButton onClick={() => onEntityFeedback(asset.id, false)} title="标记为非该实体并作为负样本">非实体</ActionButton></>}
        {asset.media_type === 'video' && (
          <><ActionButton onClick={detectShots} title="检测镜头">{isDetecting ? '切分中…' : '切分'}</ActionButton>{onViewSegments && <ActionButton onClick={() => onViewSegments(asset.id, matchingSegmentIds)} title={matchingSegmentIds?.length ? `查看 ${matchingSegmentIds.length} 个匹配镜头片段` : '查看镜头片段'}>片段</ActionButton>}</>
        )}
      </div>
    </div>
  );
}

function ActionButton({
  children,
  onClick,
  title,
}: {
  children: React.ReactNode;
  onClick: () => void;
  title: string;
}) {
  return (
    <button
      onClick={onClick}
      title={title}
      className="rounded px-2 py-1 text-xs hover:bg-neutral-200 dark:hover:bg-neutral-800"
    >
      {children}
    </button>
  );
}
