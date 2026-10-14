"use client";

import { Transcript, TranscriptSegmentData } from '@/types';
import { TranscriptView } from '@/components/TranscriptView';
import { VirtualizedTranscriptView } from '@/components/VirtualizedTranscriptView';
import { TranscriptButtonGroup } from './TranscriptButtonGroup';
import { SpeakerRenameDialog } from '@/components/SpeakerRenameDialog';
import { invoke } from '@tauri-apps/api/core';
import { useCallback, useEffect, useMemo, useState } from 'react';

interface TranscriptPanelProps {
  transcripts: Transcript[];
  customPrompt: string;
  onPromptChange: (value: string) => void;
  onCopyTranscript: () => void;
  onOpenMeetingFolder: () => Promise<void>;
  isRecording: boolean;
  disableAutoScroll?: boolean;

  // Optional pagination props (when using virtualization)
  usePagination?: boolean;
  segments?: TranscriptSegmentData[];
  hasMore?: boolean;
  isLoadingMore?: boolean;
  totalCount?: number;
  loadedCount?: number;
  onLoadMore?: () => void;

  // Retranscription props
  meetingId?: string;
  meetingFolderPath?: string | null;
  onRefetchTranscripts?: () => Promise<void>;
}

export function TranscriptPanel({
  transcripts,
  customPrompt,
  onPromptChange,
  onCopyTranscript,
  onOpenMeetingFolder,
  isRecording,
  disableAutoScroll = false,
  usePagination = false,
  segments,
  hasMore,
  isLoadingMore,
  totalCount,
  loadedCount,
  onLoadMore,
  meetingId,
  meetingFolderPath,
  onRefetchTranscripts,
}: TranscriptPanelProps) {
  // Which speaker label (if any) is being renamed via SpeakerRenameDialog.
  const [renameSpeaker, setRenameSpeaker] = useState<string | null>(null);

  // Near-match voice suggestions for unnamed speakers, keyed by label (e.g. "Speaker 2").
  // Best-effort: an empty/failed fetch just means no hints are shown.
  const [suggestions, setSuggestions] = useState<Record<string, { name: string; score: number }>>({});
  const fetchSuggestions = useCallback(async () => {
    if (!meetingId) {
      setSuggestions({});
      return;
    }
    try {
      const result = await invoke<Record<string, { name: string; score: number }>>(
        'diarization_get_suggestions',
        { meetingId }
      );
      setSuggestions(result);
    } catch {
      setSuggestions({});
    }
  }, [meetingId]);
  useEffect(() => {
    void fetchSuggestions();
  }, [fetchSuggestions]);

  // Convert transcripts to segments if pagination is not used but we want virtualization
  const convertedSegments = useMemo(() => {
    if (usePagination && segments) {
      return segments;
    }
    // Convert transcripts to segments for virtualization
    return transcripts.map(t => ({
      id: t.id,
      timestamp: t.audio_start_time ?? 0,
      endTime: t.audio_end_time,
      text: t.text,
      confidence: t.confidence,
      speaker: t.speaker,
    }));
  }, [transcripts, usePagination, segments]);

  // Already-assigned real speaker names in this meeting, offered as a picklist
  // when renaming a different speaker (so the user can merge into an existing person).
  const existingSpeakerNames = useMemo(
    () =>
      Array.from(
        new Set(
          convertedSegments
            .map((s) => (s.speaker ?? '').trim())
            .filter((n) => n.length > 0 && !/^Speaker \d+$/.test(n))
        )
      ),
    [convertedSegments]
  );

  return (
    <div className="flex h-full min-w-0 w-full bg-white flex-col relative @container">
      {/* Title area */}
      <div className="p-4 border-b border-gray-200">
        <TranscriptButtonGroup
          transcriptCount={usePagination ? (totalCount ?? convertedSegments.length) : (transcripts?.length || 0)}
          onCopyTranscript={onCopyTranscript}
          onOpenMeetingFolder={onOpenMeetingFolder}
          meetingId={meetingId}
          meetingFolderPath={meetingFolderPath}
          onRefetchTranscripts={onRefetchTranscripts}
        />
      </div>

      {/* Transcript content - use virtualized view for better performance */}
      <div className="flex-1 overflow-hidden pb-4">
        <VirtualizedTranscriptView
          segments={convertedSegments}
          isRecording={isRecording}
          isPaused={false}
          isProcessing={false}
          isStopping={false}
          enableStreaming={false}
          showConfidence={true}
          disableAutoScroll={disableAutoScroll}
          hasMore={hasMore}
          isLoadingMore={isLoadingMore}
          totalCount={totalCount}
          loadedCount={loadedCount}
          onLoadMore={onLoadMore}
          onSpeakerClick={meetingId ? (label) => setRenameSpeaker(label) : undefined}
          suggestions={suggestions}
        />
      </div>

      {/* Custom prompt input at bottom of transcript section */}
      {!isRecording && convertedSegments.length > 0 && (
        <div className="p-1 border-t border-gray-200">
          <textarea
            placeholder="Add context for AI summary. For example people involved, meeting overview, objective etc..."
            className="w-full px-3 py-2 border border-gray-200 rounded-md text-sm focus:outline-none focus:ring-1 focus:ring-blue-500 focus:border-blue-500 bg-white shadow-sm min-h-[80px] resize-y"
            value={customPrompt}
            onChange={(e) => onPromptChange(e.target.value)}
          />
        </div>
      )}

      {renameSpeaker && meetingId && (
        <SpeakerRenameDialog
          meetingId={meetingId}
          speakerLabel={renameSpeaker}
          existingNames={existingSpeakerNames}
          suggestedName={suggestions[renameSpeaker]?.name}
          onClose={() => setRenameSpeaker(null)}
          onRenamed={async () => {
            setRenameSpeaker(null);
            await onRefetchTranscripts?.();
            await fetchSuggestions();
          }}
        />
      )}
    </div>
  );
}
