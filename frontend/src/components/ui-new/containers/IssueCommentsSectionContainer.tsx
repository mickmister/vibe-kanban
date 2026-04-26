import { useMemo, useCallback, useState, useRef } from 'react';
import { useDropzone } from 'react-dropzone';
import { useTranslation } from 'react-i18next';
import { IssueProvider, useIssueContext } from '@/contexts/remote/IssueContext';
import { useOrgContext } from '@/contexts/remote/OrgContext';
import { useProjectContext } from '@/contexts/remote/ProjectContext';
import { useCurrentUser } from '@/hooks/auth/useCurrentUser';
import { useAzureAttachments } from '@/hooks/useAzureAttachments';
import { commitCommentAttachments, deleteAttachment } from '@/lib/remoteApi';
import { extractAttachmentIds } from '@/lib/attachmentUtils';
import {
  IssueCommentsSection,
  type IssueCommentData,
  type ReactionGroup,
} from '@/components/ui-new/views/IssueCommentsSection';
import type { WYSIWYGEditorRef } from '@/components/ui/wysiwyg';
import { MemberRole } from 'shared/remote-types';

interface IssueCommentsSectionContainerProps {
  issueId: string;
}

function buildReplyQuote(
  authorName: string,
  message: string,
  quotePrefix: string
) {
  const firstLine = message.split('\n')[0].trim();
  const truncatedLine =
    firstLine.length > 100 ? `${firstLine.slice(0, 100)}...` : firstLine;

  return `> ${authorName} ${quotePrefix}\n> ${truncatedLine}`;
}

/**
 * Container that wraps IssueCommentsSection with IssueProvider.
 * Manages comment data transformation, mutations, and UI state.
 */
export function IssueCommentsSectionContainer({
  issueId,
}: IssueCommentsSectionContainerProps) {
  return (
    <IssueProvider issueId={issueId}>
      <IssueCommentsSectionContent />
    </IssueProvider>
  );
}

function IssueCommentsSectionContent() {
  const { t } = useTranslation('common');
  const { membersWithProfilesById } = useOrgContext();
  const { projectId } = useProjectContext();
  const issueContext = useIssueContext();
  const { data: currentUser } = useCurrentUser();
  const currentUserId = currentUser?.user_id ?? '';

  const currentUserMember = currentUserId
    ? membersWithProfilesById.get(currentUserId)
    : undefined;
  const isCurrentUserAdmin = currentUserMember?.role === MemberRole.ADMIN;

  const commentEditorRef = useRef<WYSIWYGEditorRef>(null);

  const [commentInput, setCommentInput] = useState('');
  const [replyTargetCommentId, setReplyTargetCommentId] = useState<
    string | null
  >(null);

  const handleCommentMarkdownInsert = useCallback((markdown: string) => {
    setCommentInput((prev) =>
      prev.trim() ? `${prev}\n\n${markdown}` : markdown
    );
  }, []);

  const {
    uploadFiles,
    getAttachmentIds,
    clearAttachments,
    isUploading,
    uploadError,
    clearUploadError,
  } = useAzureAttachments({
    projectId,
    onMarkdownInsert: handleCommentMarkdownInsert,
  });

  const { getRootProps, getInputProps, isDragActive, open } = useDropzone({
    onDrop: (acceptedFiles) => {
      if (acceptedFiles.length > 0) uploadFiles(acceptedFiles);
    },
    noClick: true,
    noKeyboard: true,
  });

  const onPasteFiles = useCallback(
    (files: File[]) => {
      if (files.length > 0) uploadFiles(files);
    },
    [uploadFiles]
  );

  const [editingCommentId, setEditingCommentId] = useState<string | null>(null);
  const [editingValue, setEditingValue] = useState('');

  const commentsData = useMemo<IssueCommentData[]>(() => {
    return issueContext.comments
      .map((comment) => {
        const author = comment.author_id
          ? membersWithProfilesById.get(comment.author_id)
          : undefined;
        const isAuthor =
          comment.author_id !== null && comment.author_id === currentUserId;
        const canModify = isAuthor || isCurrentUserAdmin;

        return {
          id: comment.id,
          authorId: comment.author_id,
          parentId: comment.parent_id,
          authorName: comment.author_id
            ? author
              ? `${author.first_name ?? ''} ${author.last_name ?? ''}`.trim() ||
                author.email ||
                t('kanban.unknownUser')
              : t('kanban.unknownUser')
            : t('kanban.deletedUser'),
          message: comment.message,
          createdAt: comment.created_at,
          author: author ?? null,
          canModify,
        };
      })
      .sort(
        (a, b) =>
          new Date(a.createdAt).getTime() - new Date(b.createdAt).getTime()
      );
  }, [
    issueContext.comments,
    membersWithProfilesById,
    currentUserId,
    isCurrentUserAdmin,
    t,
  ]);

  const commentsDataById = useMemo(
    () => new Map(commentsData.map((comment) => [comment.id, comment])),
    [commentsData]
  );
  const replyTargetComment = useMemo(
    () =>
      replyTargetCommentId
        ? commentsDataById.get(replyTargetCommentId) ?? null
        : null,
    [commentsDataById, replyTargetCommentId]
  );

  const reactionsByCommentId = useMemo(() => {
    const result = new Map<string, ReactionGroup[]>();

    for (const comment of commentsData) {
      const commentReactions = issueContext.getReactionsForComment(comment.id);
      const emojiMap = new Map<
        string,
        {
          count: number;
          hasReacted: boolean;
          reactionId: string | undefined;
          userIds: string[];
        }
      >();

      for (const reaction of commentReactions) {
        const existing = emojiMap.get(reaction.emoji);
        const isCurrentUser = reaction.user_id === currentUserId;

        if (existing) {
          existing.count++;
          existing.userIds.push(reaction.user_id);
          if (isCurrentUser) {
            existing.hasReacted = true;
            existing.reactionId = reaction.id;
          }
        } else {
          emojiMap.set(reaction.emoji, {
            count: 1,
            hasReacted: isCurrentUser,
            reactionId: isCurrentUser ? reaction.id : undefined,
            userIds: [reaction.user_id],
          });
        }
      }

      const groups: ReactionGroup[] = Array.from(emojiMap.entries()).map(
        ([emoji, data]) => ({
          emoji,
          count: data.count,
          hasReacted: data.hasReacted,
          reactionId: data.reactionId,
          userNames: data.userIds.map((userId) => {
            const member = membersWithProfilesById.get(userId);
            return member
              ? `${member.first_name ?? ''} ${member.last_name ?? ''}`.trim() ||
                  member.email ||
                  t('kanban.unknownUser')
              : t('kanban.unknownUser');
          }),
        })
      );

      result.set(comment.id, groups);
    }

    return result;
  }, [commentsData, issueContext, currentUserId, membersWithProfilesById, t]);

  const handleSubmitComment = useCallback(async () => {
    if (!commentInput.trim()) return;

    const message = commentInput.trim();
    const { persisted } = issueContext.insertComment({
      issue_id: issueContext.issueId,
      message,
      parent_id: replyTargetComment?.id ?? null,
    });
    setCommentInput('');
    setReplyTargetCommentId(null);

    const allUploadedIds = getAttachmentIds();
    if (allUploadedIds.length > 0) {
      const referencedIds = extractAttachmentIds(message);
      const idsToCommit = allUploadedIds.filter((id) => referencedIds.has(id));
      const idsToDelete = allUploadedIds.filter((id) => !referencedIds.has(id));

      if (idsToCommit.length > 0) {
        try {
          const confirmedComment = await persisted;
          await commitCommentAttachments(confirmedComment.id, {
            attachment_ids: idsToCommit,
          });
        } catch (err) {
          console.error('Failed to commit comment attachments:', err);
        }
      }

      for (const id of idsToDelete) {
        deleteAttachment(id).catch((err) =>
          console.error('Failed to delete abandoned attachment:', err)
        );
      }
    }

    clearAttachments();
  }, [
    commentInput,
    issueContext,
    replyTargetComment,
    getAttachmentIds,
    clearAttachments,
  ]);

  const handleStartEdit = useCallback(
    (commentId: string) => {
      const comment = commentsDataById.get(commentId);
      if (comment) {
        setEditingCommentId(commentId);
        setEditingValue(comment.message);
      }
    },
    [commentsDataById]
  );

  const handleSaveEdit = useCallback(() => {
    if (!editingCommentId || !editingValue.trim()) return;

    issueContext.updateComment(editingCommentId, {
      message: editingValue.trim(),
    });
    setEditingCommentId(null);
    setEditingValue('');
  }, [editingCommentId, editingValue, issueContext]);

  const handleCancelEdit = useCallback(() => {
    setEditingCommentId(null);
    setEditingValue('');
  }, []);

  const handleDeleteComment = useCallback(
    (id: string) => {
      issueContext.removeComment(id);
    },
    [issueContext]
  );

  const handleToggleReaction = useCallback(
    (commentId: string, emoji: string) => {
      const reactions = issueContext.getReactionsForComment(commentId);
      const existingReaction = reactions.find(
        (reaction) => reaction.user_id === currentUserId && reaction.emoji === emoji
      );

      if (existingReaction) {
        issueContext.removeReaction(existingReaction.id);
      } else {
        issueContext.insertReaction({
          comment_id: commentId,
          emoji,
        });
      }
    },
    [issueContext, currentUserId]
  );

  const handleReply = useCallback(
    (comment: IssueCommentData) => {
      const quotePrefix = t('kanban.replyQuotePrefix');
      const nextQuote = buildReplyQuote(
        comment.authorName,
        comment.message,
        quotePrefix
      );
      const previousReplyTarget = replyTargetCommentId
        ? commentsDataById.get(replyTargetCommentId)
        : null;
      const previousQuote = previousReplyTarget
        ? buildReplyQuote(
            previousReplyTarget.authorName,
            previousReplyTarget.message,
            quotePrefix
          )
        : null;

      setReplyTargetCommentId(comment.id);
      setCommentInput((current) => {
        const trimmed = current.trim();
        if (!trimmed) {
          return `${nextQuote}\n\n`;
        }

        if (trimmed === nextQuote || trimmed.startsWith(`${nextQuote}\n`)) {
          return current;
        }

        if (previousQuote && trimmed.startsWith(previousQuote)) {
          const remainder = trimmed.slice(previousQuote.length).trimStart();
          return remainder ? `${nextQuote}\n\n${remainder}` : `${nextQuote}\n\n`;
        }

        return current;
      });

      setTimeout(() => {
        commentEditorRef.current?.focus();
      }, 0);
    },
    [commentsDataById, replyTargetCommentId, t]
  );

  const handleCancelReply = useCallback(() => {
    if (!replyTargetComment) {
      setReplyTargetCommentId(null);
      return;
    }

    const replyQuote = buildReplyQuote(
      replyTargetComment.authorName,
      replyTargetComment.message,
      t('kanban.replyQuotePrefix')
    );

    setReplyTargetCommentId(null);
    setCommentInput((current) => {
      const trimmed = current.trim();
      if (!trimmed) {
        return current;
      }

      if (trimmed === replyQuote) {
        return '';
      }

      if (trimmed.startsWith(replyQuote)) {
        return trimmed.slice(replyQuote.length).trimStart();
      }

      return current;
    });
  }, [replyTargetComment, t]);

  return (
    <IssueCommentsSection
      comments={commentsData}
      commentInput={commentInput}
      onCommentInputChange={setCommentInput}
      onSubmitComment={handleSubmitComment}
      editingCommentId={editingCommentId}
      editingValue={editingValue}
      onEditingValueChange={setEditingValue}
      onStartEdit={handleStartEdit}
      onSaveEdit={handleSaveEdit}
      onCancelEdit={handleCancelEdit}
      onDeleteComment={handleDeleteComment}
      reactionsByCommentId={reactionsByCommentId}
      onToggleReaction={handleToggleReaction}
      onReply={handleReply}
      replyTargetComment={replyTargetComment}
      onCancelReply={handleCancelReply}
      isLoading={issueContext.isLoading}
      commentEditorRef={commentEditorRef}
      onPasteFiles={onPasteFiles}
      dropzoneProps={{ getRootProps, getInputProps, isDragActive }}
      onBrowseAttachment={open}
      isUploading={isUploading}
      attachmentError={uploadError}
      onDismissAttachmentError={clearUploadError}
    />
  );
}
