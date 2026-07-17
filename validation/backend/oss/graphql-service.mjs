import { createServer } from 'node:http';
import { createRequire } from 'node:module';

const require = createRequire(process.env.GRAPHQL_PACKAGE_JSON);
const { buildSchema, graphql } = require('graphql');
const schema = buildSchema(`
  interface Entity { id: ID! }
  type User implements Entity { id: ID!, name: String!, nickname: String }
  type Service implements Entity { id: ID!, endpoint: String! }
  union SearchResult = User | Service
  type Query {
    node(kind: String!): Entity
    search: [SearchResult]!
    nullableNode: Entity
    explode: String
  }
`);
const rootValue = {
  node: ({ kind }) =>
    kind === 'user'
      ? { __typename: 'User', id: 'u1', name: 'Ada', nickname: null }
      : { __typename: 'Service', id: 's1', endpoint: 'https://example.test' },
  search: () => [
    { __typename: 'User', id: 'u1', name: 'Ada', nickname: null },
    { __typename: 'Service', id: 's1', endpoint: 'https://example.test' },
  ],
  nullableNode: () => null,
  explode: () => {
    throw new Error('intentional dogfood resolver error');
  },
};
const server = createServer(async (request, response) => {
  if (request.method !== 'POST' || request.url !== '/graphql') {
    response.writeHead(404).end();
    return;
  }
  let body = '';
  for await (const chunk of request) body += chunk;
  const payload = JSON.parse(body);
  const result = await graphql({
    schema,
    source: payload.query,
    variableValues: payload.variables,
    rootValue,
  });
  response.writeHead(200, { 'content-type': 'application/json' });
  response.end(JSON.stringify(result));
});
server.listen(Number(process.env.PORT || 18788), '127.0.0.1', () => {
  process.stdout.write(`graphql shapes ready ${server.address().port}\n`);
});
