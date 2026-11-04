import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';
import { Link } from 'react-router-dom';
import { open } from '@tauri-apps/plugin-dialog';
import { createLibrary, getAppStats, startScan } from '@/api';
import type { IndexProfile } from '@/types';

export default function Home() {
  const queryClient = useQueryClient();
  const [path, setPath] = useState('');
  const [profile, setProfile] = useState<IndexProfile>('balanced');
  const { data: stats, isLoading } = useQuery({
    queryKey: ['appStats'],
    queryFn: getAppStats,
  });
  const startImport = useMutation({
    mutationFn: async () => {
      const name = path.split(/[\\/]/).filter(Boolean).pop() ?? '我的素材库';
      const library = await createLibrary({ name, root_path: path, index_profile: profile });
      await startScan(library.id);
    },
    onSuccess: () => { queryClient.invalidateQueries({ queryKey: ['appStats'] }); queryClient.invalidateQueries({ queryKey: ['libraries'] }); queryClient.invalidateQueries({ queryKey: ['jobs'] }); },
  });
  const pickFolder = async () => { const selected = await open({ directory: true, multiple: false }); if (typeof selected === 'string') setPath(selected); };

  return (
    <div className="p-8">
      <div className="mx-auto max-w-4xl">
        <h1 className="mb-2 text-3xl font-bold">说出你要的镜头，直接回到创作。</h1>
        <p className="mb-8 text-neutral-600 dark:text-neutral-400">
          SceneWeaver 帮助你用自然语言、参考图和组合条件快速找到本地图片与视频素材。
        </p>

        <div className="mb-8 grid grid-cols-2 gap-4 sm:grid-cols-4">
          <StatCard label="素材库" value={isLoading ? '-' : stats?.library_count ?? 0} />
          <StatCard label="素材总数" value={isLoading ? '-' : stats?.asset_count ?? 0} />
          <StatCard label="图片" value={isLoading ? '-' : stats?.image_count ?? 0} />
          <StatCard label="视频" value={isLoading ? '-' : stats?.video_count ?? 0} />
        </div>

        {!isLoading && stats?.library_count === 0 && <section className="mb-8 rounded-xl border border-brand-200 bg-white p-5 dark:border-brand-900 dark:bg-neutral-950"><h2 className="font-semibold">三步开始建立本地素材库</h2><p className="mt-1 text-sm text-neutral-500">1. 选择你的素材文件夹 · 2. 选择索引模式 · 3. 后台开始扫描。不会复制、移动或上传源素材。</p><div className="mt-4 flex flex-col gap-3 sm:flex-row"><button type="button" onClick={() => void pickFolder()} className="rounded-lg border px-3 py-2 text-left text-sm">{path || '1. 选择文件夹'}</button><select value={profile} onChange={(event) => setProfile(event.target.value as IndexProfile)} className="rounded-lg border px-3 py-2 text-sm"><option value="quick">快速（低频采样）</option><option value="balanced">平衡（逐帧切分）</option><option value="precise">精确（多帧质量分析）</option></select><button disabled={!path || startImport.isPending} onClick={() => startImport.mutate()} className="rounded-lg bg-brand-600 px-4 py-2 text-white disabled:opacity-50">{startImport.isPending ? '正在开始…' : '3. 开始后台扫描'}</button></div>{startImport.isError && <p className="mt-2 text-sm text-red-600">导入失败：{startImport.error.message}</p>}{startImport.isSuccess && <p className="mt-2 text-sm text-emerald-700">扫描任务已在后台开始，可立即前往素材库或任务中心查看。</p>}</section>}

        <div className="flex gap-3">
          <Link
            to="/libraries"
            className="rounded-lg bg-brand-600 px-5 py-2.5 font-medium text-white hover:bg-brand-700"
          >
            添加素材库
          </Link>
          <Link
            to="/settings"
            className="rounded-lg border border-neutral-300 bg-white px-5 py-2.5 font-medium text-neutral-700 hover:bg-neutral-50 dark:border-neutral-700 dark:bg-neutral-800 dark:text-neutral-200"
          >
            打开设置
          </Link>
        </div>
      </div>
    </div>
  );
}

function StatCard({ label, value }: { label: string; value: number | string }) {
  return (
    <div className="rounded-xl border border-neutral-200 bg-white p-4 dark:border-neutral-800 dark:bg-neutral-950">
      <div className="text-2xl font-bold">{value}</div>
      <div className="text-sm text-neutral-500">{label}</div>
    </div>
  );
}
