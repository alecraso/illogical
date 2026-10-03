#!/bin/sh
# S23: the same full read of a GitHub PR as one GraphQL query (read-only), for M38's REST-vs-GraphQL question.
#   ./github-graphql.sh cli cli 14519
owner=$1 name=$2 n=$3
exec gh api graphql -i -F owner="$owner" -F name="$name" -F n="$n" -f query='
query($owner:String!,$name:String!,$n:Int!){
  rateLimit{cost remaining used}
  repository(owner:$owner,name:$name){ pullRequest(number:$n){
    number title state isDraft merged mergeable reviewDecision author{login} baseRefName headRefName headRefOid baseRefOid
    headRepository{nameWithOwner} isCrossRepository updatedAt
    reviewRequests(first:20){nodes{requestedReviewer{__typename ... on User{login} ... on Team{slug}}}}
    reviews(first:50){nodes{author{login} state submittedAt body}}
    commits(last:1){nodes{commit{oid statusCheckRollup{state contexts(first:100){nodes{__typename
      ... on CheckRun{name status conclusion detailsUrl checkSuite{workflowRun{databaseId}}}
      ... on StatusContext{context state targetUrl}}}}}}}
    timelineItems(last:50){totalCount nodes{__typename ... on IssueComment{author{login} createdAt body}
      ... on PullRequestReview{author{login} state} ... on ReviewRequestedEvent{createdAt requestedReviewer{... on User{login} ... on Team{slug}}}
      ... on MergedEvent{createdAt} ... on HeadRefForcePushedEvent{createdAt}}}
    files(first:100){nodes{path additions deletions changeType}}
  }}}'
