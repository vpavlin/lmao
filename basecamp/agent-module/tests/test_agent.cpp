#include <logos_test.h>
#include "../src/agent_impl.h"

LOGOS_TEST(echo_returns_prefixed_input) {
    AgentImpl impl;
    LOGOS_ASSERT_EQ(impl.echo("hello"), std::string("echo: hello"));
}

LOGOS_TEST(echo_handles_empty_input) {
    AgentImpl impl;
    LOGOS_ASSERT_EQ(impl.echo(""), std::string("echo: "));
}
