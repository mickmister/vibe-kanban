import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { cn } from '@/lib/utils';
import {
  DotsThreeIcon,
  SmileyIcon,
  ArrowUpIcon,
  PencilSimpleIcon,
  TrashIcon,
  ArrowBendUpLeftIcon,
  PaperclipIcon,
} from '@phosphor-icons/react';
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip';
import { ErrorAlert } from '@/components/ui-new/primitives/ErrorAlert';
import { UserAvatar } from '@/components/ui-new/primitives/UserAvatar';
import { CollapsibleSectionHeader } from '@/components/ui-new/primitives/CollapsibleSectionHeader';
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
} from '@/components/ui-new/primitives/Dropdown';
import { EmojiPicker } from '@/components/ui-new/primitives/EmojiPicker';
import WYSIWYGEditor, { type WYSIWYGEditorRef } from '@/components/ui/wysiwyg';
import { formatRelativeTime } from '@/utils/date';
import type { OrganizationMemberWithProfile } from 'shared/types';
import type { PersistKey } from '@/stores/useUiPreferencesStore';

export interface IssueCommentData {
  id: string;
  authorId: string | null;
  parentId: string | null;
  authorName: string;
  message: string;
  createdAt: string;
  author?: OrganizationMemberWithProfile | null;
  canModify: boolean;
}

export interface ReactionGroup {
  emoji: string;
  count: number;
  hasReacted: boolean;
  reactionId: string | undefined;
  userNames: string[];
}

interface DropzoneProps {
  getRootProps: () => Record<string, unknown>;
  getInputProps: () => Record<string, unknown>;
  isDragActive: boolean;
}

interface IssueCommentsSectionProps {
  comments: IssueCommentData[];
  commentInput: string;
  onCommentInputChange: (value: string) => void;
  onSubmitComment: () => void;
  editingCommentId: string | null;
  editingValue: string;
  onEditingValueChange: (value: string) => void;
  onStartEdit: (commentId: string) => void;
  onSaveEdit: () => void;
  onCancelEdit: () => void;
  onDeleteComment: (id: string) => void;
  reactionsByCommentId: Map<string, ReactionGroup[]>;
  onToggleReaction: (commentId: string, emoji: string) => void;
  onReply: (comment: IssueCommentData) => void;
  replyTargetComment: IssueCommentData | null;
  onCancelReply: () => void;
  isLoading?: boolean;
  commentEditorRef?: React.Ref<WYSIWYGEditorRef>;
  onPasteFiles?: (files: File[]) => void;
  dropzoneProps?: DropzoneProps;
  onBrowseAttachment?: () => void;
  isUploading?: boolean;
  attachmentError?: string | null;
  onDismissAttachmentError?: () => void;
}

export function IssueCommentsSection({
  comments,
  commentInput,
  onCommentInputChange,
  onSubmitComment,
  editingCommentId,
  editingValue,
  onEditingValueChange,
  onStartEdit,
  onSaveEdit,
  onCancelEdit,
  onDeleteComment,
  reactionsByCommentId,
  onToggleReaction,
  onReply,
  replyTargetComment,
  onCancelReply,
  isLoading,
  commentEditorRef,
  onPasteFiles,
  dropzoneProps,
  onBrowseAttachment,
  isUploading,
  attachmentError,
  onDismissAttachmentError,
}: IssueCommentsSectionProps) {
  const { t } = useTranslation('common');
  const commentIds = useMemo(
    () => new Set(comments.map((comment) => comment.id)),
    [comments]
  );
  const rootComments = useMemo(
    () =>
      comments.filter(
        (comment) =>
          !comment.parentId || !commentIds.has(comment.parentId)
      ),
    [commentIds, comments]
  );
  const repliesByParentId = useMemo(() => {
    const map = new Map<string, IssueCommentData[]>();

    for (const comment of comments) {
      if (!comment.parentId || !commentIds.has(comment.parentId)) {
        continue;
      }

      const replies = map.get(comment.parentId) ?? [];
      replies.push(comment);
      map.set(comment.parentId, replies);
    }

    return map;
  }, [commentIds, comments]);

  return (
    <CollapsibleSectionHeader
      title={t('kanban.comments')}
      persistKey={'kanban-issue-comments' as PersistKey}
      defaultExpanded={true}
      actions={[]}
    >
      <div className="p-base flex flex-col gap-base border-t">
        {isLoading ? (
          <div className="flex flex-col gap-double animate-pulse">
            <div className="h-4 bg-secondary rounded w-3/4" />
            <div className="h-4 bg-secondary rounded w-1/2" />
          </div>
        ) : rootComments.length === 0 ? (
          <p className="text-low">{t('kanban.noCommentsYet')}</p>
        ) : (
          rootComments.map((comment) => (
            <CommentThread
              key={comment.id}
              comment={comment}
              repliesByParentId={repliesByParentId}
              editingCommentId={editingCommentId}
              editingValue={editingValue}
              onEditingValueChange={onEditingValueChange}
              onStartEdit={onStartEdit}
              onSaveEdit={onSaveEdit}
              onCancelEdit={onCancelEdit}
              onDeleteComment={onDeleteComment}
              reactionsByCommentId={reactionsByCommentId}
              onToggleReaction={onToggleReaction}
              onReply={onReply}
            />
          ))
        )}

        <div
          {...dropzoneProps?.getRootProps()}
          className="relative flex flex-col gap-double bg-secondary border border-border rounded-sm p-double"
        >
          <input {...dropzoneProps?.getInputProps()} />
          {replyTargetComment && (
            <div className="flex items-center justify-between gap-base rounded-sm border border-border bg-panel px-base py-half">
              <span className="text-sm text-low">
                {t('kanban.replyingTo', { name: replyTargetComment.authorName })}
              </span>
              <button
                type="button"
                onClick={onCancelReply}
                className="text-sm text-low hover:text-normal transition-colors"
              >
                {t('buttons.cancel')}
              </button>
            </div>
          )}
          <WYSIWYGEditor
            ref={commentEditorRef}
            value={commentInput}
            onChange={onCommentInputChange}
            placeholder={t('kanban.enterCommentPlaceholder')}
            className="min-h-[20px]"
            onCmdEnter={onSubmitComment}
            onPasteFiles={onPasteFiles}
            autoFocus={false}
          />
          {attachmentError && (
            <div className="mb-half">
              <ErrorAlert
                message={attachmentError}
                onDismiss={onDismissAttachmentError}
                dismissLabel={t('buttons.close')}
              />
            </div>
          )}
          <div className="flex items-center justify-end gap-half">
            {onBrowseAttachment && (
              <TooltipProvider>
                <Tooltip>
                  <TooltipTrigger asChild>
                    <button
                      type="button"
                      onClick={onBrowseAttachment}
                      title={t('kanban.attachFile')}
                      className={cn(
                        'size-[22px] rounded-full bg-panel border border-border',
                        'flex items-center justify-center',
                        'text-low hover:text-normal transition-colors'
                      )}
                      aria-label={t('kanban.attachFile')}
                    >
                      <PaperclipIcon size={12} />
                    </button>
                  </TooltipTrigger>
                  <TooltipContent>{t('kanban.attachFileHint')}</TooltipContent>
                </Tooltip>
              </TooltipProvider>
            )}
            <button
              type="button"
              onClick={onSubmitComment}
              disabled={!commentInput.trim() || isUploading}
              className={cn(
                'size-[22px] rounded-full bg-panel border border-border',
                'flex items-center justify-center',
                'text-high hover:bg-secondary transition-colors',
                'disabled:opacity-50 disabled:cursor-not-allowed'
              )}
            >
              <ArrowUpIcon size={12} weight="bold" />
            </button>
          </div>
          {dropzoneProps?.isDragActive && (
            <div className="absolute inset-0 z-50 bg-primary/80 backdrop-blur-sm border-2 border-dashed border-brand rounded flex items-center justify-center">
              <p className="text-sm font-medium text-high">
                {t('kanban.dropFilesHere')}
              </p>
            </div>
          )}
        </div>
      </div>
    </CollapsibleSectionHeader>
  );
}

interface CommentThreadProps {
  comment: IssueCommentData;
  repliesByParentId: Map<string, IssueCommentData[]>;
  editingCommentId: string | null;
  editingValue: string;
  onEditingValueChange: (value: string) => void;
  onStartEdit: (commentId: string) => void;
  onSaveEdit: () => void;
  onCancelEdit: () => void;
  onDeleteComment: (id: string) => void;
  reactionsByCommentId: Map<string, ReactionGroup[]>;
  onToggleReaction: (commentId: string, emoji: string) => void;
  onReply: (comment: IssueCommentData) => void;
  depth?: number;
}

function CommentThread({
  comment,
  repliesByParentId,
  editingCommentId,
  editingValue,
  onEditingValueChange,
  onStartEdit,
  onSaveEdit,
  onCancelEdit,
  onDeleteComment,
  reactionsByCommentId,
  onToggleReaction,
  onReply,
  depth = 0,
}: CommentThreadProps) {
  const replies = repliesByParentId.get(comment.id) ?? [];

  return (
    <div className="flex flex-col gap-base">
      <CommentItem
        comment={comment}
        isEditing={editingCommentId === comment.id}
        editValue={editingCommentId === comment.id ? editingValue : ''}
        onEditValueChange={onEditingValueChange}
        onStartEdit={() => onStartEdit(comment.id)}
        onSaveEdit={onSaveEdit}
        onCancelEdit={onCancelEdit}
        onDelete={() => onDeleteComment(comment.id)}
        reactions={reactionsByCommentId.get(comment.id) ?? []}
        onToggleReaction={(emoji) => onToggleReaction(comment.id, emoji)}
        onReply={() => onReply(comment)}
      />
      {replies.length > 0 && (
        <div
          className={cn(
            'flex flex-col gap-base border-l border-border pl-double',
            depth > 0 ? 'ml-base' : 'ml-double'
          )}
        >
          {replies.map((reply) => (
            <CommentThread
              key={reply.id}
              comment={reply}
              repliesByParentId={repliesByParentId}
              editingCommentId={editingCommentId}
              editingValue={editingValue}
              onEditingValueChange={onEditingValueChange}
              onStartEdit={onStartEdit}
              onSaveEdit={onSaveEdit}
              onCancelEdit={onCancelEdit}
              onDeleteComment={onDeleteComment}
              reactionsByCommentId={reactionsByCommentId}
              onToggleReaction={onToggleReaction}
              onReply={onReply}
              depth={depth + 1}
            />
          ))}
        </div>
      )}
    </div>
  );
}

interface CommentItemProps {
  comment: IssueCommentData;
  isEditing: boolean;
  editValue: string;
  onEditValueChange: (value: string) => void;
  onStartEdit: () => void;
  onSaveEdit: () => void;
  onCancelEdit: () => void;
  onDelete: () => void;
  reactions: ReactionGroup[];
  onToggleReaction: (emoji: string) => void;
  onReply: () => void;
}

function CommentItem({
  comment,
  isEditing,
  editValue,
  onEditValueChange,
  onStartEdit,
  onSaveEdit,
  onCancelEdit,
  onDelete,
  reactions,
  onToggleReaction,
  onReply,
}: CommentItemProps) {
  const { t } = useTranslation('common');
  const timeAgo = formatRelativeTime(comment.createdAt);

  return (
    <div className="flex flex-col gap-base">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-base">
          {comment.author ? (
            <UserAvatar user={comment.author} className="size-4" />
          ) : (
            <div className="size-4 rounded-full bg-secondary border border-border flex items-center justify-center text-[10px] text-low">
              {comment.authorName.charAt(0).toUpperCase()}
            </div>
          )}
          <span className="font-medium text-low">{comment.authorName}</span>
          <span className="font-medium text-low">·</span>
          <span className="font-light text-low">{timeAgo}</span>
        </div>
        {comment.canModify && (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <button className="size-5 flex items-center justify-center text-low hover:text-normal">
                <DotsThreeIcon size={16} weight="bold" />
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem icon={PencilSimpleIcon} onSelect={onStartEdit}>
                {t('buttons.edit')}
              </DropdownMenuItem>
              <DropdownMenuItem
                icon={TrashIcon}
                variant="destructive"
                onSelect={onDelete}
              >
                {t('buttons.delete')}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        )}
      </div>

      {isEditing ? (
        <div className="flex flex-col gap-half bg-primary border border-border rounded-sm p-double">
          <WYSIWYGEditor
            value={editValue}
            onChange={onEditValueChange}
            autoFocus
            onCmdEnter={onSaveEdit}
            className="min-h-[40px]"
          />
          <div className="flex gap-half justify-end">
            <button
              type="button"
              onClick={onCancelEdit}
              className="px-base py-half text-low hover:text-normal"
            >
              {t('buttons.cancel')}
            </button>
            <button
              type="button"
              onClick={onSaveEdit}
              disabled={!editValue.trim()}
              className={cn(
                'px-base py-half bg-brand text-on-brand rounded-sm',
                'hover:bg-brand-hover disabled:opacity-50'
              )}
            >
              {t('buttons.save')}
            </button>
          </div>
        </div>
      ) : (
        <WYSIWYGEditor
          value={comment.message}
          disabled
          className="text-normal"
        />
      )}

      <div className="flex items-center gap-base flex-wrap">
        <TooltipProvider>
          {reactions.map((reaction) => (
            <Tooltip key={reaction.emoji}>
              <TooltipTrigger asChild>
                <button
                  type="button"
                  onClick={() => onToggleReaction(reaction.emoji)}
                  className={cn(
                    'flex items-center gap-half px-base py-half rounded-sm',
                    'border transition-colors',
                    reaction.hasReacted
                      ? 'bg-brand/10 border-brand text-brand'
                      : 'bg-secondary border-border text-low hover:text-normal'
                  )}
                >
                  <span className="color-emoji">{reaction.emoji}</span>
                  <span className="text-xs">{reaction.count}</span>
                </button>
              </TooltipTrigger>
              <TooltipContent className="bg-panel border border-border">
                {reaction.userNames.join(', ')}
              </TooltipContent>
            </Tooltip>
          ))}
        </TooltipProvider>

        <EmojiPicker onSelect={onToggleReaction}>
          <button
            type="button"
            className="size-6 flex items-center justify-center text-low hover:text-normal rounded-sm hover:bg-secondary transition-colors"
          >
            <SmileyIcon size={16} />
          </button>
        </EmojiPicker>

        <button
          type="button"
          onClick={onReply}
          className="flex items-center gap-half text-low hover:text-normal transition-colors"
        >
          <ArrowBendUpLeftIcon size={16} />
          <span className="font-light">{t('buttons.reply')}</span>
        </button>
      </div>
    </div>
  );
}
