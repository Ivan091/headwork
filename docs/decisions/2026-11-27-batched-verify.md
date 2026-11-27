# 2026-11-27: Verify batches per node, or per node and file

The verify node checks a group of findings in one subagent instead of one finding each: `each = "node"` groups an
input node's findings, `each = "file"` groups them per input node and file (findings naming no file form one group per
node). The `code` flow uses `node` at level 2 and `file` at level 3. A group is formed only from the findings of an
input node that is over, so it never changes once it is ready: an instance handed out stays the same set of findings,
and verify can start on one node's findings while slower nodes still run (pipelined fan-out). A file group across all
nodes would grow every time another node finished, so it could start only after the last node, which is the barrier
this change removes; grouping per (node, file) keeps the pipelining at the price of two verifiers when two nodes flag
the same file.

An instance's `item` is the smallest finding id of its group, so `review_nodes` keeps its schema and its primary key;
`review_next` lists the group as `checks`, and `review_node_done` takes `verdicts` naming each of them once (a group of
one still takes `verdict`). Each finding gets its own verdict, and the prompt tells the verifier to judge each on its
own, so batching does not let one finding's verdict carry another's.
