public class MyCallsignDvGatewayData
{
	private int m_a;

	public int OffsetProgrammableMemoryAddress
	{
		set
		{
			this.m_a = value;
		}
	}

	public string MyCallsignDvGateway
	{
		get { return string.Empty; }
	}

	public string MemoDvGateway
	{
		get { return string.Empty; }
	}

	public void b(n7 A_0, int A_1)
	{
		int num = 331784 + this.m_a + 12 * A_1;
		A_0.d(MyCallsignDvGateway, num, 8);
		A_0.d(MemoDvGateway, num + 8, 4);
	}
}
