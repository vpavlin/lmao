cmake_minimum_required(VERSION 3.14)
project(AgentTests LANGUAGES CXX)

include(LogosTest)

logos_test(
    NAME agent_tests
    MODULE_SOURCES ../src/agent_impl.cpp
    TEST_SOURCES
        main.cpp
        test_agent.cpp
)
