import { useEffect } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { listen } from '@tauri-apps/api/event';
import { listJobs, pauseJob, resumeJob, cancelJob, retryJob } from '@/api';
import type { Job, ScanProgress } from '@/types';
import { scanMetrics } from '@/lib/scanMetrics';

export default function Jobs() {
  const queryClient = useQueryClient();
  const { data: jobs, isLoading } = useQuery({
    queryKey: ['jobs'],
    queryFn: listJobs,
    refetchInterval: 1000,
  });

  const pause = useMutation({
    mutationFn: pauseJob,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['jobs'] }),
  });
  const resume = useMutation({
    mutationFn: resumeJob,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['jobs'] }),
  });
  const cancel = useMutation({
    mutationFn: cancelJob,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['jobs'] }),
  });
  const retry = useMutation({
    mutationFn: retryJob,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['jobs'] }),
  });
  const actionError = pause.error ?? resume.error ?? cancel.error ?? retry.error;

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<ScanProgress>('scan:progress', (event) => {
      queryClient.setQueryData<Job[]>(['jobs'], (current) =>
        current?.map((job) =>
          job.id === event.payload.job_id
            ? {
                ...job,
                status: event.payload.status,
                progress: event.payload.progress >= 0 ? event.payload.progress : job.progress,
                current_step: event.payload.current_step || job.current_step,
                checkpoint_json: JSON.stringify({
                  processed: event.payload.processed,
                  total: event.payload.total,
                  errors: event.payload.errors,
                }),
              }
            : job,
        ),
      );
    }).then((stopListening) => {
      unlisten = stopListening;
    });
    return () => unlisten?.();
  }, [queryClient]);

  return (
    <div className="p-8">
      <h1 className="mb-6 text-2xl font-bold">任务中心</h1>
      {actionError && <p role="alert" className="mb-3 text-sm text-red-600">任务操作失败：{actionError.message}</p>}
      {isLoading ? (
        <p className="text-neutral-500">加载中…</p>
      ) : jobs?.length === 0 ? (
        <p className="text-neutral-500">暂无任务。</p>
      ) : (
        <div className="space-y-3">
          {jobs?.map((job) => (
            <JobRow
              key={job.id}
              job={job}
              onPause={() => pause.mutate(job.id)}
              onResume={() => resume.mutate(job.id)}
              onCancel={() => cancel.mutate(job.id)}
              onRetry={() => retry.mutate(job.id)}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function JobRow({
  job,
  onPause,
  onResume,
  onCancel,
  onRetry,
}: {
  job: Job;
  onPause: () => void;
  onResume: () => void;
  onCancel: () => void;
  onRetry: () => void;
}) {
  const metrics = scanMetrics(job.checkpoint_json);
  const statusMap: Record<Job['status'], string> = {
    pending: '等待中',
    running: '运行中',
    paused: '已暂停',
    completed: '已完成',
    failed: '失败',
    cancelled: '已取消',
  };

  return (
    <div className="rounded-xl border border-neutral-200 bg-white p-4 dark:border-neutral-800 dark:bg-neutral-950">
      <div className="mb-2 flex items-center justify-between">
        <div>
          <span className="font-medium">{job.job_type}</span>
          <span className="ml-2 text-sm text-neutral-500">{statusMap[job.status]}</span>
        </div>
        <div className="flex gap-2">
          {job.status === 'running' && (
            <button data-testid={`pause-job-${job.id}`} onClick={onPause} className="rounded border px-2 py-1 text-xs hover:bg-neutral-100 dark:hover:bg-neutral-800">
              暂停
            </button>
          )}
          {job.status === 'paused' && (
            <button data-testid={`resume-job-${job.id}`} onClick={onResume} className="rounded border px-2 py-1 text-xs hover:bg-neutral-100 dark:hover:bg-neutral-800">
              恢复
            </button>
          )}
          {(job.status === 'pending' || job.status === 'running' || job.status === 'paused') && (
            <button onClick={onCancel} className="rounded border px-2 py-1 text-xs hover:bg-red-50 text-red-600 dark:hover:bg-red-950">
              取消
            </button>
          )}
          {(job.status === 'failed' || job.status === 'cancelled') && (
            <button onClick={onRetry} className="rounded border px-2 py-1 text-xs hover:bg-neutral-100 dark:hover:bg-neutral-800">
              重试
            </button>
          )}
        </div>
      </div>
      <div className="mb-1 text-sm text-neutral-600 dark:text-neutral-400">
        {job.current_step || '准备中'}
      </div>
      <div className="h-2 w-full overflow-hidden rounded-full bg-neutral-200 dark:bg-neutral-800">
        <div
          className="h-full bg-brand-600 transition-all"
          style={{ width: `${Math.round(job.progress * 100)}%` }}
        />
      </div>
      <p className="mt-1 text-xs text-neutral-500">{Math.round(job.progress * 100)}% 完成</p>
      {metrics.total > 0 && (
        <p className="mt-1 text-xs text-neutral-500">
          已处理 {metrics.processed} / {metrics.total} · 错误 {metrics.errors}
        </p>
      )}
      {job.error_message && (
        <p className="mt-2 text-xs text-red-600">{job.error_message}</p>
      )}
    </div>
  );
}
