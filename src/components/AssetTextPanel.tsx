import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { open } from '@tauri-apps/plugin-dialog';
import { analyzeAssetText, copyToClipboard, deleteAssetText, listAssetText } from '@/api';
import { formatTimecode } from '@/lib/mediaFormat';
import type { Asset, AssetTextSource } from '@/types';

export function AssetTextPanel({ asset, onClose }: { asset: Asset; onClose: () => void }) {
  const [source, setSource] = useState<AssetTextSource>(asset.media_type === 'audio' ? 'speech' : 'ocr');
  const [language, setLanguage] = useState('eng');
  const queryClient = useQueryClient();
  const key = ['assetText', asset.id, source];
  const cues = useQuery({ queryKey: key, queryFn: () => listAssetText(asset.id, source) });
  const analysis = useMutation({
    mutationFn: async () => {
      const modelPath = source === 'speech'
        ? await open({ multiple: false, filters: [{ name: 'Whisper 模型', extensions: ['bin'] }] })
        : null;
      if (source === 'speech' && typeof modelPath !== 'string') return undefined;
      return analyzeAssetText(asset.id, source, language, typeof modelPath === 'string' ? modelPath : null);
    },
    onSuccess: (result) => {
      if (result) queryClient.setQueryData(key, result);
    },
  });
  const remove = useMutation({
    mutationFn: () => deleteAssetText(asset.id, source),
    onSuccess: () => {
      queryClient.setQueryData(key, []);
      analysis.reset();
    },
  });
  const busy = analysis.isPending || remove.isPending;
  const error = analysis.error ?? remove.error ?? cues.error;

  return (
    <section className="mt-6 rounded-xl border bg-white p-4 dark:bg-neutral-950" aria-label="本地素材文字">
      <div className="mb-3 flex items-start justify-between gap-3">
        <h2 className="font-semibold">本地文字 · {asset.file_name}</h2>
        <button onClick={onClose} className="text-sm text-neutral-500">关闭</button>
      </div>
      {asset.media_type === 'video' && (
        <select
          aria-label="文字来源"
          value={source}
          disabled={busy}
          onChange={(event) => { setSource(event.target.value as AssetTextSource); analysis.reset(); remove.reset(); }}
          className="mb-3 rounded border px-2 py-1 text-sm dark:bg-neutral-900"
        >
          <option value="ocr">画面文字</option>
          <option value="speech">语音转写</option>
        </select>
      )}
      <p className="mb-3 text-sm text-neutral-500">
        {source === 'ocr'
          ? '使用本机 Tesseract 和已安装的语言包。视频识别最多 24 张已生成的镜头关键帧。'
          : '使用支持 whisper 滤镜的本机 FFmpeg 和你选择的 Whisper 模型，在本机转写第一条音轨。'}
      </p>
      <div className="flex flex-wrap gap-2 text-sm">
        {source === 'ocr' && (
          <select aria-label="OCR 语言" value={language} onChange={(event) => setLanguage(event.target.value)} disabled={busy} className="rounded border px-2 dark:bg-neutral-900">
            <option value="eng">英文</option>
            <option value="chi_sim">简体中文</option>
            <option value="jpn">日文</option>
            <option value="eng+chi_sim">中英混合</option>
          </select>
        )}
        <button onClick={() => analysis.mutate()} disabled={busy || asset.status !== 'indexed'} className="rounded bg-brand-600 px-3 py-1 text-white disabled:opacity-50">
          {analysis.isPending ? '正在分析…' : source === 'ocr' ? '识别文字' : '选择模型并转写'}
        </button>
        {!!cues.data?.length && <button onClick={() => remove.mutate()} disabled={busy} className="rounded border px-3 py-1">移除文字索引</button>}
      </div>
      {error && <p role="alert" className="mt-3 text-sm text-red-600">{error.message}</p>}
      {analysis.data?.length === 0 && <p className="mt-3 text-sm text-neutral-500">本次分析未识别到文字。</p>}
      {!!cues.data?.length && <ol className="mt-3 max-h-80 space-y-2 overflow-y-auto text-sm">
        {cues.data.map((cue, index) => (
          <li key={index} className="rounded border p-2">
            {cue.start_ms !== null && (
              <button className="mb-1 text-xs text-brand-600" title="复制入点时间码" onClick={() => copyToClipboard(formatTimecode(cue.start_ms!))}>
                {formatTimecode(cue.start_ms)}{cue.end_ms !== null && ` – ${formatTimecode(cue.end_ms)}`}
              </button>
            )}
            <p className="whitespace-pre-wrap">{cue.text}</p>
          </li>
        ))}
      </ol>}
    </section>
  );
}
