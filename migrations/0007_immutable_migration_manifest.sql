ALTER TABLE schema_migrations ADD COLUMN sha256 TEXT;

UPDATE schema_migrations
SET sha256 = CASE version
    WHEN 1 THEN '8f90ef761cc47bfa6d48d9be6d13504231457764c8efc51d3cf65ba0bb337c66'
    WHEN 2 THEN '5fe0a4fed78e12fc11d722203cc678ba0e55a0d25abc48c5c0f34b4ad95284ec'
    WHEN 3 THEN '331c82be7c8b09e23eccceed3a41971c9cd2f54c29c36ae4e1bebbc812c73399'
    WHEN 4 THEN 'b786240b81dc5ff615aa6f25e80249dda971bdd8001efc3bc04c21b0855e0f08'
    WHEN 5 THEN '1958764d5c8f9180a5bf13f2be66f46b8ab1e8034252aebe061c020f967f0b56'
    WHEN 6 THEN '17edad3e23b121895c919a9f22f3607b110765660a49a076f29a027f1083b26f'
END;

CREATE TRIGGER schema_migrations_immutable_update
BEFORE UPDATE ON schema_migrations
BEGIN
    SELECT RAISE(ABORT, 'schema migration records are immutable');
END;

CREATE TRIGGER schema_migrations_immutable_delete
BEFORE DELETE ON schema_migrations
BEGIN
    SELECT RAISE(ABORT, 'schema migration records are immutable');
END;
