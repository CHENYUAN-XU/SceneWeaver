import { useParams } from 'react-router-dom';
import { useQuery } from '@tanstack/react-query';
import { addSegmentToDefaultSelects, addSegmentToSelectCollection, copyToClipboard, getLibrary, listAssets, listSegments, listSelectCollections, segmentPreviewDataUrl } from '@/api';
import { MediaGrid } from '@/components/MediaGrid';
import { AssetTextPanel } from '@/components/AssetTextPanel';
import { formatDuration, formatTimecode } from '@/lib/mediaFormat';
import { useState } from 'react';
import type { Asset } from '@/types';

export default function LibraryDetail() {
  const { id } = useParams<{ id: string }>();
  const [segmentAssetId, setSegmentAssetId] = useState<string | null>(null);
  const [textAsset, setTextAsset] = useState<Asset | null>(null);
  const { data: library, isLoading: libLoading } = useQuery({
    queryKey: ['library', id],
    queryFn: () => getLibrary(id!),
    enabled: !!id,
  });
  const { data: segments, isLoading: segmentsLoading } = useQuery({ queryKey: ['segments', segmentAssetId], queryFn: () => listSegments(segmentAssetId!), enabled: segmentAssetId !== null });
  const { data: assets, isLoading: assetsLoading } = useQuery({
    queryKey: ['assets', id],
    queryFn: () => listAssets(id!),
    enabled: !!id,
  });
  const { data: selectCollections } = useQuery({ queryKey: ['selectCollections'], queryFn: listSelectCollections });

  if (libLoading) return <div className="p-8">加载中…</div>;
  if (!library) return <div className="p-8">素材库不存在</div>;

  return (
    <div className="p-8">
      <div className="mb-6">
        <h1 className="text-2xl font-bold">{library.name}</h1>
        <p className="text-sm text-neutral-500" title={library.root_path}>
          {library.root_path}
        </p>
      </div>

      <div className="mb-4 flex items-center justify-between">
        <h2 className="font-semibold">
          媒体文件 {assets ? `(${assets.length})` : ''}
        </h2>
        <span className="text-sm text-neutral-500">
          状态：{library.status === 'scanning' ? '扫描中' : library.status === 'paused' ? '已暂停' : library.status === 'error' ? '错误' : '空闲'}
        </span>
      </div>

      {assetsLoading ? (
        <p className="text-neutral-500">加载媒体中…</p>
      ) : (
        <MediaGrid assets={assets ?? []} onViewSegments={setSegmentAssetId} onViewText={setTextAsset} />
      )}
      {segmentAssetId && <section className="mt-8 rounded-xl border bg-white p-4 dark:bg-neutral-950"><div className="mb-3 flex items-center justify-between"><h2 className="font-semibold">镜头片段</h2><button onClick={() => setSegmentAssetId(null)} className="text-sm text-neutral-500">关闭</button></div>{segmentsLoading && <p className="text-sm text-neutral-500">加载片段中…</p>}{segments?.length === 0 && <p className="text-sm text-neutral-500">尚未切分镜头。请先在对应视频卡片点击“切分”。</p>}<div className="space-y-3">{segments?.map((segment) => <SegmentCard key={segment.id} assetId={segmentAssetId} segment={segment} collections={selectCollections ?? []} />)}</div></section>}
      {textAsset && <AssetTextPanel key={textAsset.id} asset={textAsset} onClose={() => setTextAsset(null)} />}
    </div>
  );
}

function SegmentCard({ assetId, segment, collections }: { assetId: string; segment: import('@/types').Segment; collections: import('@/types').SelectCollection[] }) {
  const [showPreview, setShowPreview] = useState(false);
  const [selected, setSelected] = useState(false);
  const [collectionId, setCollectionId] = useState('default');
  const preview = useQuery({ queryKey: ['segmentPreview', assetId, segment.id], queryFn: () => segmentPreviewDataUrl(assetId, segment.id), enabled: showPreview });
  const addToSelects = async () => {
    if (collectionId === 'default') await addSegmentToDefaultSelects(assetId, segment.id);
    else await addSegmentToSelectCollection(collectionId, assetId, segment.id);
    setSelected(true);
  };
  const quality = segment.quality_score === null ? '待生成' : `${Math.round(segment.quality_score * 100)}%`;
  return <article className="flex flex-col gap-3 rounded-lg border p-3 text-sm sm:flex-row"><div className="flex h-24 w-40 shrink-0 items-center justify-center overflow-hidden rounded bg-neutral-100 dark:bg-neutral-900">{segment.thumbnail_data_url ? <img src={segment.thumbnail_data_url} alt={`片段 ${segment.segment_index + 1} 关键帧`} className="h-full w-full object-cover" /> : <span className="text-xs text-neutral-500">未生成关键帧</span>}</div><div className="min-w-0 flex-1"><p className="font-medium">#{segment.segment_index + 1} · {formatDuration(segment.duration_ms)}</p><p className="mt-1 text-neutral-500">{formatTimecode(segment.start_ms)} — {formatTimecode(segment.end_ms)}</p><p className="mt-1 text-xs text-neutral-500">质量 {quality} · 黑帧 {formatMetric(segment.black_frame_score)} · 模糊 {formatMetric(segment.blur_score)}</p><div className="mt-3 flex flex-wrap gap-2"><button onClick={() => copyToClipboard(formatTimecode(segment.start_ms))} className="rounded border px-2 py-1 text-xs">复制入点时间码</button><button onClick={() => setShowPreview((value) => !value)} disabled={!segment.preview_path} className="rounded border px-2 py-1 text-xs disabled:opacity-50">{showPreview ? '隐藏预览' : '播放短预览'}</button><label className="flex items-center gap-1 text-xs">加入<select value={collectionId} onChange={(event) => { setCollectionId(event.target.value); setSelected(false); }} aria-label="加入片段到选片集合" className="rounded border px-2 py-1"><option value="default">我的选片（默认）</option>{collections.filter((collection) => collection.name !== '我的选片').map((collection) => <option key={collection.id} value={collection.id}>{collection.name}</option>)}</select></label><button onClick={addToSelects} className="rounded border px-2 py-1 text-xs">{selected ? '已加入选片' : '加入选片'}</button></div>{showPreview && (preview.isLoading ? <p className="mt-2 text-xs text-neutral-500">加载预览中…</p> : preview.data ? <video controls autoPlay muted src={preview.data} className="mt-3 max-h-56 w-full rounded bg-black" /> : <p className="mt-2 text-xs text-neutral-500">短预览不可用。</p>)}</div></article>;
}

function formatMetric(value: number | null | undefined) {
  return value === null || value === undefined ? '—' : `${Math.round(value * 100)}%`;
}
